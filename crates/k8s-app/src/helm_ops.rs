//! Helm operations that change a cluster, shared by every front end.
//!
//! Each one refuses a protected cluster, runs helm against a kubeconfig
//! narrowed to the one context, and leaves an audit entry whatever the outcome.
//! Confirming with the user is the front end's job.

use std::{fmt::Display, sync::Arc};

use k8s_core::ClusterHandle;
use k8s_helm::{
    ChartResult, Helm, Repository, UpgradeDiff, UpgradeOptions,
    cli::{self, ChartRef, ReleaseTarget},
    store,
};

use crate::{audit::AuditEntry, state::AppCore};

impl AppCore {
    /// Append an action and its outcome to the audit log.
    pub fn record_audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        self.audit.record(&AuditEntry {
            timestamp: k8s_openapi::jiff::Timestamp::now().to_string(),
            cluster: cluster.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            outcome: match result {
                Ok(_) => "ok".to_string(),
                Err(err) => err.to_string(),
            },
        });
    }
}

/// What every write needs before helm can run.
struct Prepared {
    handle: Arc<ClusterHandle>,
    helm: Arc<Helm>,
    kubeconfig: String,
}

fn prepare(core: &AppCore, cluster: &str, namespace: &str) -> Result<Prepared, String> {
    core.ensure_writable(cluster)?;
    Ok(Prepared {
        handle: core.clusters.require(cluster).map_err(|e| e.to_string())?,
        helm: core.helm().map_err(|e| e.to_string())?,
        kubeconfig: core
            .clusters
            .minified_kubeconfig(cluster, Some(namespace))
            .map_err(|e| e.to_string())?,
    })
}

/// Roll a release back to an earlier revision. Returns helm's output.
pub async fn rollback(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    release: &str,
    revision: i64,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, namespace)?;
    let result = prepared
        .helm
        .rollback(
            &prepared.handle,
            &prepared.kubeconfig,
            release,
            namespace,
            revision,
        )
        .await;
    core.record_audit(
        cluster,
        "helmRollback",
        &format!("{namespace}/{release}"),
        &result,
    );
    result.map_err(|e| e.to_string())
}

/// Remove a release and the objects it installed. Returns helm's output.
pub async fn uninstall(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    release: &str,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, namespace)?;
    let result = prepared
        .helm
        .uninstall(
            &prepared.handle,
            &prepared.kubeconfig,
            release,
            namespace,
            false,
        )
        .await;
    core.record_audit(
        cluster,
        "helmUninstall",
        &format!("{namespace}/{release}"),
        &result,
    );
    result.map_err(|e| e.to_string())
}

/// What an upgrade needs: which release, which chart at which version, and the
/// values to install it with.
#[derive(Debug, Clone)]
pub struct UpgradeRequest {
    pub namespace: String,
    pub release: String,
    /// `repo/chart`, a local path, or an `oci://` reference.
    pub chart: String,
    pub version: Option<String>,
    pub values: String,
    pub options: UpgradeOptions,
}

impl UpgradeRequest {
    fn target(&self) -> ReleaseTarget<'_> {
        ReleaseTarget {
            release: &self.release,
            namespace: &self.namespace,
        }
    }

    fn chart_ref(&self) -> ChartRef<'_> {
        ChartRef {
            reference: &self.chart,
            version: self.version.as_deref(),
        }
    }
}

/// What an upgrade would change, without changing anything.
///
/// Renders the proposed chart and compares it with the manifest helm recorded
/// for the installed revision, which is the comparison the `helm-diff` plugin
/// makes, without needing the plugin. It writes nothing, so a protected
/// cluster may still be previewed.
pub async fn preview_upgrade(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<UpgradeDiff, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let helm = core.helm().map_err(|e| e.to_string())?;
    let kubeconfig = core
        .clusters
        .minified_kubeconfig(cluster, Some(&request.namespace))
        .map_err(|e| e.to_string())?;

    let proposed = helm
        .template(
            &handle,
            &kubeconfig,
            &request.target(),
            &request.chart_ref(),
            &request.values,
        )
        .await
        .map_err(|e| e.to_string())?;

    // A release that does not exist yet has no installed manifest, so the diff
    // is everything the install would create.
    let current = store::detail(&handle, &request.namespace, &request.release, None)
        .await
        .map(|detail| detail.manifest)
        .unwrap_or_default();

    Ok(cli::diff_manifests(&current, &proposed, &request.release))
}

/// Upgrade a release, installing it if it does not exist. Returns helm's output.
pub async fn upgrade(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<String, String> {
    run_upgrade(core, cluster, request, "helmUpgrade").await
}

async fn run_upgrade(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
    action: &str,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, &request.namespace)?;
    let result = prepared
        .helm
        .upgrade(
            &prepared.handle,
            &prepared.kubeconfig,
            &request.target(),
            &request.chart_ref(),
            &request.values,
            &request.options,
        )
        .await;
    core.record_audit(
        cluster,
        action,
        &format!("{}/{}", request.namespace, request.release),
        &result,
    );
    result.map_err(|e| e.to_string())
}

/// Release names helm accepts: at most 53 characters of lowercase letters,
/// digits, `-` and `.`, beginning and ending with a letter or digit.
pub fn check_release_name(name: &str) -> Result<(), &'static str> {
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if name.is_empty() {
        return Err("Give the release a name.");
    }
    if name.len() > 53 {
        return Err("A release name can be at most 53 characters.");
    }
    if !name.chars().all(|c| edge(c) || matches!(c, '-' | '.')) {
        return Err("A release name uses lowercase letters, digits, - and . only.");
    }
    let (first, last) = (name.chars().next(), name.chars().next_back());
    if !first.is_some_and(edge) || !last.is_some_and(edge) {
        return Err("A release name must start and end with a letter or digit.");
    }
    Ok(())
}

/// Namespace names Kubernetes accepts: a DNS label.
pub fn check_namespace(namespace: &str) -> Result<(), &'static str> {
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if namespace.is_empty() {
        return Err("Choose a namespace.");
    }
    if namespace.len() > 63 {
        return Err("A namespace can be at most 63 characters.");
    }
    if !namespace.chars().all(|c| edge(c) || c == '-') {
        return Err("A namespace uses lowercase letters, digits and - only.");
    }
    if !namespace.starts_with(edge) || !namespace.ends_with(edge) {
        return Err("A namespace must start and end with a letter or digit.");
    }
    Ok(())
}

/// Fail if a release of this name already lives in the namespace.
///
/// `helm upgrade --install` quietly upgrades an existing release, which is not
/// what someone installing a new chart meant, and could replace a running
/// application with another chart.
async fn ensure_new_release(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<(), String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let existing = store::list(&handle, Some(&request.namespace))
        .await
        .map_err(|e| e.to_string())?;
    if existing
        .iter()
        .any(|release| release.name == request.release)
    {
        return Err(format!(
            "A release named {} already exists in {}. Open it under Releases and use its \
             Upgrade tab instead.",
            request.release, request.namespace
        ));
    }
    Ok(())
}

/// What installing a chart would create, without creating anything.
pub async fn preview_install(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<UpgradeDiff, String> {
    ensure_new_release(core, cluster, request).await?;
    preview_upgrade(core, cluster, request).await
}

/// Install a chart as a new release. Returns helm's output.
pub async fn install(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<String, String> {
    core.ensure_writable(cluster)?;
    ensure_new_release(core, cluster, request).await?;
    run_upgrade(core, cluster, request, "helmInstall").await
}

/// A chart's default values, as YAML: what an install starts from.
pub async fn chart_values(
    core: &AppCore,
    chart: &str,
    version: Option<&str>,
) -> Result<String, String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.show_values(chart, version)
        .await
        .map_err(|e| e.to_string())
}

// ------------------------------------------------------------ repositories
//
// Repositories live in the user's own helm configuration, so what is added or
// removed here is also what the `helm` command line sees. None of this touches
// a cluster, so none of it is audited or refused for a protected context.

pub async fn list_repos(core: &AppCore) -> Result<Vec<Repository>, String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.repo_list().await.map_err(|e| e.to_string())
}

/// Add a repository and fetch its index, so its charts are searchable at once.
pub async fn add_repo(core: &AppCore, name: &str, url: &str) -> Result<(), String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.repo_add(name, url).await.map_err(|e| e.to_string())
}

pub async fn remove_repo(core: &AppCore, name: &str) -> Result<(), String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.repo_remove(name).await.map_err(|e| e.to_string())
}

/// Refresh every repository's index.
pub async fn update_repos(core: &AppCore) -> Result<String, String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.repo_update().await.map_err(|e| e.to_string())
}

/// Charts across all repositories; an empty query lists them all.
pub async fn search_charts(core: &AppCore, query: &str) -> Result<Vec<ChartResult>, String> {
    let helm = core.helm().map_err(|e| e.to_string())?;
    helm.search(query).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{check_namespace, check_release_name};

    #[test]
    fn release_names_follow_helms_rules() {
        for ok in ["web", "my-app", "app.v2", "a", "9lives"] {
            assert!(check_release_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Web", "my_app", "-app", "app-", ".app", "has space"] {
            assert!(check_release_name(bad).is_err(), "{bad}");
        }
        assert!(check_release_name(&"a".repeat(53)).is_ok());
        assert!(check_release_name(&"a".repeat(54)).is_err());
    }

    #[test]
    fn namespaces_are_dns_labels() {
        for ok in ["default", "kube-system", "team-a", "9"] {
            assert!(check_namespace(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Team", "team_a", "-a", "a-", "a.b"] {
            assert!(check_namespace(bad).is_err(), "{bad}");
        }
        assert!(check_namespace(&"a".repeat(63)).is_ok());
        assert!(check_namespace(&"a".repeat(64)).is_err());
    }
}
