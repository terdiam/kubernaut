//! The helm binary, for the operations that render charts.
//!
//! Reads go through [`crate::store`]; this module is only for the things that
//! genuinely need helm: rendering templates, running hooks, and writing release
//! history. Reimplementing Go templates and Sprig in Rust would produce subtly
//! different output from the tool everyone else uses, which is worse than
//! shelling out.
//!
//! Note that repository operations write to the user's own helm configuration
//! (`~/.config/helm`), so a repo added here is also visible to their CLI.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use dashmap::DashMap;
use k8s_core::cluster::ClusterHandle;
use serde_json::Value;

use crate::model::{
    ChartResult, DocumentChange, HelmError, HelmInfo, Repository, Result, UpgradeDiff,
};

/// Long enough for a chart download plus rendering on a slow link, short enough
/// that a hung process does not wedge the UI forever.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// A per-cluster kubeconfig on disk, deleted when the cluster is dropped.
struct ClusterAccess {
    path: PathBuf,
    context: String,
}

impl Drop for ClusterAccess {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path) {
            tracing::debug!(path = %self.path.display(), %err, "could not remove helm kubeconfig");
        }
    }
}

pub struct Helm {
    binary: PathBuf,
    bundled: bool,
    access: DashMap<String, Arc<ClusterAccess>>,
}

impl Helm {
    /// Find helm: the copy shipped with the app first, then the user's own.
    ///
    /// Preferring the bundled binary keeps behaviour identical across machines;
    /// falling back means a developer who already has helm is not blocked when
    /// the sidecar is missing from a dev build.
    pub fn resolve(sidecar_dir: Option<&Path>) -> Result<Self> {
        let candidate =
            sidecar_dir.map(|dir| dir.join(if cfg!(windows) { "helm.exe" } else { "helm" }));

        if let Some(path) = candidate.filter(|path| path.is_file()) {
            return Ok(Self {
                binary: path,
                bundled: true,
                access: DashMap::new(),
            });
        }

        match k8s_core::paths::which("helm") {
            Some(path) => Ok(Self {
                binary: path,
                bundled: false,
                access: DashMap::new(),
            }),
            None => Err(HelmError::NoBinary(
                "no bundled helm and none on PATH. Install helm, or add its directory in Settings."
                    .into(),
            )),
        }
    }

    pub async fn info(&self) -> Result<HelmInfo> {
        let version = self
            .raw(&["version", "--short"], None)
            .await?
            .trim()
            .to_string();
        Ok(HelmInfo {
            path: self.binary.display().to_string(),
            version,
            bundled: self.bundled,
        })
    }

    pub fn forget_cluster(&self, cluster: &str) {
        self.access.remove(cluster);
    }

    /// Kubeconfig for a cluster, created on first use.
    fn access_for(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
    ) -> Result<Arc<ClusterAccess>> {
        if let Some(existing) = self.access.get(&cluster.id) {
            return Ok(existing.clone());
        }
        let path = k8s_core::kubeconfig::write_private(kubeconfig)?;
        let access = Arc::new(ClusterAccess {
            path,
            context: cluster.id.clone(),
        });
        self.access.insert(cluster.id.clone(), access.clone());
        Ok(access)
    }

    /// Run helm without cluster context (repo and search operations).
    async fn raw(&self, args: &[&str], stdin: Option<&str>) -> Result<String> {
        self.execute(args, None, stdin).await
    }

    /// Run helm against a cluster.
    async fn run(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<String> {
        let access = self.access_for(cluster, kubeconfig)?;
        self.execute(args, Some(&access), stdin).await
    }

    async fn execute(
        &self,
        args: &[&str],
        access: Option<&ClusterAccess>,
        stdin: Option<&str>,
    ) -> Result<String> {
        let mut command = tokio::process::Command::new(&self.binary);
        command.args(args);
        if let Some(access) = access {
            command
                .env("KUBECONFIG", &access.path)
                .args(["--kube-context", &access.context]);
        }
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| HelmError::NoBinary(err.to_string()))?;

        if let Some(input) = stdin
            && let Some(mut handle) = child.stdin.take()
        {
            use tokio::io::AsyncWriteExt;
            handle.write_all(input.as_bytes()).await?;
            handle.shutdown().await?;
        }

        let output = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(HelmError::Command {
                    command: args.first().unwrap_or(&"helm").to_string(),
                    message: format!("timed out after {}s", COMMAND_TIMEOUT.as_secs()),
                });
            }
        };

        if !output.status.success() {
            // helm puts the actionable message on stderr; stdout is usually
            // empty on failure.
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(HelmError::Command {
                command: args.first().unwrap_or(&"helm").to_string(),
                message: if message.is_empty() {
                    format!("exited with {}", output.status)
                } else {
                    message
                },
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    // ------------------------------------------------------ repositories

    pub async fn repo_list(&self) -> Result<Vec<Repository>> {
        // An empty repo file makes helm exit non-zero; that is "none", not an
        // error the user needs to see.
        let output = match self.raw(&["repo", "list", "-o", "json"], None).await {
            Ok(output) => output,
            Err(HelmError::Command { message, .. }) if message.contains("no repositories") => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };
        Ok(serde_json::from_str(&output).unwrap_or_default())
    }

    pub async fn repo_add(&self, name: &str, url: &str) -> Result<()> {
        self.raw(&["repo", "add", name, url, "--force-update"], None)
            .await?;
        self.raw(&["repo", "update", name], None).await?;
        Ok(())
    }

    pub async fn repo_remove(&self, name: &str) -> Result<()> {
        self.raw(&["repo", "remove", name], None).await?;
        Ok(())
    }

    pub async fn repo_update(&self) -> Result<String> {
        self.raw(&["repo", "update"], None).await
    }

    pub async fn search(&self, query: &str) -> Result<Vec<ChartResult>> {
        let args = if query.trim().is_empty() {
            vec!["search", "repo", "-o", "json"]
        } else {
            vec!["search", "repo", query, "-o", "json"]
        };
        let output = match self.raw(&args, None).await {
            Ok(output) => output,
            Err(HelmError::Command { message, .. }) if message.contains("no repositories") => {
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };

        let parsed: Vec<Value> = serde_json::from_str(&output).unwrap_or_default();
        Ok(parsed
            .into_iter()
            .filter_map(|entry| {
                Some(ChartResult {
                    name: entry.get("name")?.as_str()?.to_string(),
                    version: entry
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    app_version: entry
                        .get("app_version")
                        .and_then(Value::as_str)
                        .map(String::from),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(String::from),
                })
            })
            .collect())
    }

    /// Default values for a chart, as YAML — what the values editor starts from.
    pub async fn show_values(&self, chart: &str, version: Option<&str>) -> Result<String> {
        let mut args = vec!["show", "values", chart];
        if let Some(version) = version {
            args.push("--version");
            args.push(version);
        }
        self.raw(&args, None).await
    }

    // ----------------------------------------------------------- releases

    /// Render a chart without touching the cluster.
    pub async fn template(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
        target: &ReleaseTarget<'_>,
        chart: &ChartRef<'_>,
        values: &str,
    ) -> Result<String> {
        let (release, namespace) = (target.release, target.namespace);
        let version = chart.version;
        let chart = chart.reference;
        let values_file = k8s_core::kubeconfig::write_private(values)?;
        let values_path = values_file.display().to_string();

        let mut args = vec![
            "template",
            release,
            chart,
            "--namespace",
            namespace,
            "--values",
            &values_path,
        ];
        if let Some(version) = version {
            args.push("--version");
            args.push(version);
        }

        let result = self.run(cluster, kubeconfig, &args, None).await;
        let _ = std::fs::remove_file(&values_file);
        result
    }

    /// Install, or upgrade if the release already exists.
    pub async fn upgrade(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
        target: &ReleaseTarget<'_>,
        chart: &ChartRef<'_>,
        values: &str,
        options: &UpgradeOptions,
    ) -> Result<String> {
        let (release, namespace) = (target.release, target.namespace);
        let version = chart.version;
        let chart = chart.reference;
        let values_file = k8s_core::kubeconfig::write_private(values)?;
        let values_path = values_file.display().to_string();
        let timeout = format!("{}s", options.timeout_seconds);

        let mut args = vec![
            "upgrade",
            release,
            chart,
            "--install",
            "--namespace",
            namespace,
            "--values",
            &values_path,
            "--timeout",
            &timeout,
        ];
        if let Some(version) = version {
            args.push("--version");
            args.push(version);
        }
        if options.create_namespace {
            args.push("--create-namespace");
        }
        if options.wait {
            args.push("--wait");
        }
        if options.atomic {
            // Atomic implies wait and rolls back on failure — the safe default
            // for an interactive upgrade.
            args.push("--atomic");
        }
        if options.reset_values {
            args.push("--reset-values");
        }
        if options.dry_run {
            args.push("--dry-run");
        }

        let result = self.run(cluster, kubeconfig, &args, None).await;
        let _ = std::fs::remove_file(&values_file);
        result
    }

    pub async fn rollback(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
        release: &str,
        namespace: &str,
        revision: i64,
    ) -> Result<String> {
        let revision = revision.to_string();
        self.run(
            cluster,
            kubeconfig,
            &[
                "rollback",
                release,
                &revision,
                "--namespace",
                namespace,
                "--wait",
            ],
            None,
        )
        .await
    }

    pub async fn uninstall(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig: &str,
        release: &str,
        namespace: &str,
        keep_history: bool,
    ) -> Result<String> {
        let mut args = vec!["uninstall", release, "--namespace", namespace];
        if keep_history {
            args.push("--keep-history");
        }
        self.run(cluster, kubeconfig, &args, None).await
    }
}

/// Which release, and where.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseTarget<'a> {
    pub release: &'a str,
    pub namespace: &'a str,
}

/// Which chart, at which version. `reference` may be `repo/chart`, a local
/// path, or an `oci://` URL — helm resolves all three.
#[derive(Debug, Clone, Copy)]
pub struct ChartRef<'a> {
    pub reference: &'a str,
    pub version: Option<&'a str>,
}

/// Flags offered in the upgrade dialog.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeOptions {
    #[serde(default)]
    pub create_namespace: bool,
    #[serde(default)]
    pub wait: bool,
    /// Roll back automatically if the upgrade fails.
    #[serde(default = "default_true")]
    pub atomic: bool,
    /// Discard previously supplied values instead of merging onto them.
    #[serde(default)]
    pub reset_values: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u32 {
    300
}

impl Default for UpgradeOptions {
    fn default() -> Self {
        Self {
            create_namespace: false,
            wait: false,
            atomic: true,
            reset_values: false,
            dry_run: false,
            timeout_seconds: default_timeout(),
        }
    }
}

/// Compare a proposed render against the manifest currently installed.
///
/// This is what `helm diff` does as a plugin; doing it here means one less
/// thing for the user to install, and the inputs are exactly what an upgrade
/// would apply. On top of the text diff it reports which objects changed, and
/// whether the only differences are Secret values the chart regenerates every
/// render — otherwise such charts always look like they are about to change.
pub fn diff_manifests(current: &str, proposed: &str, release: &str) -> UpgradeDiff {
    let documents = compare_documents(current, proposed);
    let changed = !documents.is_empty();
    let generated_only = changed && documents.iter().all(|doc| doc.generated_only);

    let unified = if changed {
        similar::TextDiff::from_lines(current, proposed)
            .unified_diff()
            .context_radius(4)
            .header(
                &format!("{release} (installed)"),
                &format!("{release} (proposed)"),
            )
            .to_string()
    } else {
        String::new()
    };

    UpgradeDiff {
        unified,
        changed,
        documents,
        generated_only,
    }
}

/// Identity of one document in a rendered manifest.
fn identify(document: &str) -> Option<(String, String)> {
    let mut kind: Option<String> = None;
    let mut name: Option<String> = None;
    let mut in_metadata = false;

    for line in document.lines() {
        if let Some(rest) = line.strip_prefix("kind:") {
            kind = Some(rest.trim().to_string());
        } else if line.starts_with("metadata:") {
            in_metadata = true;
        } else if in_metadata && line.starts_with("  name:") {
            name = line
                .split_once(':')
                .map(|(_, value)| value.trim().trim_matches('"').to_string());
            // The first `name` under `metadata` is the object's own; later ones
            // belong to nested structures.
            in_metadata = false;
        } else if !line.starts_with(' ')
            && !line.is_empty()
            && line.starts_with(|c: char| c.is_alphabetic())
            && line != "metadata:"
        {
            in_metadata = false;
        }
    }

    Some((kind?, name?))
}

/// Split a rendered manifest into documents keyed by kind and name.
fn split_documents(manifest: &str) -> Vec<((String, String), String)> {
    manifest
        .split("\n---")
        .filter_map(|document| {
            let trimmed = document.trim();
            if trimmed.is_empty() {
                return None;
            }
            identify(trimmed).map(|key| (key, trimmed.to_string()))
        })
        .collect()
}

/// Lines of a Secret document that carry generated material.
///
/// Restricted to the keys charts actually randomise; a changed `metadata` or a
/// changed key *name* is a real change and must not be excused.
fn only_secret_values_differ(before: &str, after: &str) -> bool {
    let value_lines = |document: &str| -> (Vec<String>, Vec<String>) {
        let mut structure = Vec::new();
        let mut values = Vec::new();
        let mut in_data = false;
        for line in document.lines() {
            if line.starts_with("data:") || line.starts_with("stringData:") {
                in_data = true;
                structure.push(line.to_string());
                continue;
            }
            if in_data && !line.starts_with("  ") {
                in_data = false;
            }
            if in_data {
                // Keep the key, drop the value: a new key is structure, a new
                // value for an existing key may be regenerated material.
                match line.split_once(':') {
                    Some((key, _)) => {
                        structure.push(format!("{key}:"));
                        values.push(line.to_string());
                    }
                    None => structure.push(line.to_string()),
                }
            } else {
                structure.push(line.to_string());
            }
        }
        (structure, values)
    };

    let (before_structure, before_values) = value_lines(before);
    let (after_structure, after_values) = value_lines(after);

    // Same shape, different values, and there were values to differ.
    before_structure == after_structure && before_values != after_values
}

fn compare_documents(current: &str, proposed: &str) -> Vec<DocumentChange> {
    let installed = split_documents(current);
    let wanted = split_documents(proposed);

    let installed_map: std::collections::HashMap<_, _> = installed.iter().cloned().collect();
    let wanted_map: std::collections::HashMap<_, _> = wanted.iter().cloned().collect();

    let mut changes: Vec<DocumentChange> = Vec::new();

    for (key, body) in &wanted {
        match installed_map.get(key) {
            None => changes.push(DocumentChange {
                kind: key.0.clone(),
                name: key.1.clone(),
                change: "added".into(),
                generated_only: false,
            }),
            Some(existing) if existing.trim() != body.trim() => {
                let generated_only = key.0 == "Secret" && only_secret_values_differ(existing, body);
                changes.push(DocumentChange {
                    kind: key.0.clone(),
                    name: key.1.clone(),
                    change: "modified".into(),
                    generated_only,
                });
            }
            Some(_) => {}
        }
    }

    for (key, _) in &installed {
        if !wanted_map.contains_key(key) {
            changes.push(DocumentChange {
                kind: key.0.clone(),
                name: key.1.clone(),
                change: "removed".into(),
                generated_only: false,
            });
        }
    }

    changes.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(replicas: u32) -> String {
        format!(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  replicas: {replicas}\n"
        )
    }

    fn secret(value: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: web-tls\ndata:\n  tls.crt: {value}\n"
        )
    }

    #[test]
    fn identical_manifests_produce_no_diff() {
        let manifest = service(1);
        let diff = diff_manifests(&manifest, &manifest, "web");
        assert!(!diff.changed);
        assert!(diff.documents.is_empty());
    }

    #[test]
    fn changed_manifests_show_both_sides() {
        let diff = diff_manifests(&service(1), &service(3), "web");
        assert!(diff.changed);
        assert!(diff.unified.contains("-  replicas: 1"), "{}", diff.unified);
        assert!(diff.unified.contains("+  replicas: 3"), "{}", diff.unified);
        assert_eq!(diff.documents.len(), 1);
        assert_eq!(diff.documents[0].change, "modified");
        assert!(!diff.documents[0].generated_only);
    }

    #[test]
    fn trailing_whitespace_is_not_a_change() {
        let diff = diff_manifests(&service(1), &format!("{}\n", service(1)), "web");
        assert!(!diff.changed);
    }

    /// Charts that call `genSelfSignedCert` render new material every time.
    /// Reporting that as a pending change trains people to ignore the diff.
    #[test]
    fn regenerated_secret_material_is_flagged_not_hidden() {
        let diff = diff_manifests(&secret("AAAA"), &secret("BBBB"), "web");
        assert!(diff.changed, "the difference is real and still reported");
        assert!(diff.generated_only, "but it is only regenerated material");
        assert_eq!(diff.documents.len(), 1);
        assert!(diff.documents[0].generated_only);
    }

    /// A *new* secret key is a real change, not regenerated material.
    #[test]
    fn added_secret_key_is_not_treated_as_generated() {
        let before = secret("AAAA");
        let after = format!("{before}  extra: QQQQ\n");
        let diff = diff_manifests(&before, &after, "web");
        assert!(diff.changed);
        assert!(!diff.generated_only, "a new key changes the shape");
    }

    /// A mix of regenerated material and a real change must not be excused.
    #[test]
    fn a_real_change_alongside_generated_material_is_not_excused() {
        let before = format!("{}\n---\n{}", secret("AAAA"), service(1));
        let after = format!("{}\n---\n{}", secret("BBBB"), service(3));
        let diff = diff_manifests(&before, &after, "web");
        assert!(diff.changed);
        assert!(!diff.generated_only);
        assert_eq!(diff.documents.len(), 2);
    }

    #[test]
    fn added_and_removed_objects_are_reported() {
        let before = service(1);
        let after = format!("{}\n---\n{}", service(1), secret("AAAA"));
        let diff = diff_manifests(&before, &after, "web");
        assert_eq!(diff.documents.len(), 1);
        assert_eq!(diff.documents[0].change, "added");

        let reversed = diff_manifests(&after, &before, "web");
        assert_eq!(reversed.documents[0].change, "removed");
    }

    #[test]
    fn atomic_is_on_by_default() {
        assert!(UpgradeOptions::default().atomic);
    }
}
