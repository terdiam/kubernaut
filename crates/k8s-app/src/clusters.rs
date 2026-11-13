//! Importing clusters.
//!
//! Imported kubeconfigs are stored in this app's own config directory and
//! merged on top of the system one at load time. `~/.kube/config` is never
//! written to: it is shared with kubectl, helm and every other tool, and a UI
//! that edits it can break workflows it knows nothing about.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Where imported kubeconfigs live.
pub fn managed_directory() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "kubernaut", "Kubernaut")
        .map(|directories| directories.config_dir().join("clusters"))
}

/// Every kubeconfig this app manages.
pub fn managed_files() -> Vec<PathBuf> {
    let Some(directory) = managed_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };

    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml" || extension == "yml")
        })
        .collect();
    files.sort();
    files
}

/// One imported file, as listed in Settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedKubeconfig {
    pub file: String,
    pub label: String,
    pub contexts: Vec<String>,
}

pub fn list() -> Vec<ManagedKubeconfig> {
    managed_files()
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let contexts = k8s_core::kubeconfig::preview(&text).unwrap_or_default();
            Some(ManagedKubeconfig {
                label: path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file: path.display().to_string(),
                contexts,
            })
        })
        .collect()
}

/// What an import would add, and what it would clash with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub contexts: Vec<String>,
    /// Context names already present. Importing these unchanged would leave two
    /// entries with one name and no way to tell which cluster a click reaches.
    pub conflicts: Vec<String>,
    /// Suggested names for the conflicting ones.
    pub suggested: BTreeMap<String, String>,
}

/// Inspect a kubeconfig document against the contexts already known.
pub fn preview(yaml: &str, existing: &[String]) -> Result<ImportPreview, String> {
    let contexts = k8s_core::kubeconfig::preview(yaml).map_err(|err| err.to_string())?;

    let conflicts: Vec<String> = contexts
        .iter()
        .filter(|name| existing.contains(name))
        .cloned()
        .collect();

    let mut suggested = BTreeMap::new();
    for name in &conflicts {
        let mut candidate = format!("{name}-imported");
        let mut counter = 2;
        while existing.contains(&candidate) || contexts.contains(&candidate) {
            candidate = format!("{name}-imported-{counter}");
            counter += 1;
        }
        suggested.insert(name.clone(), candidate);
    }

    Ok(ImportPreview {
        contexts,
        conflicts,
        suggested,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    /// Raw kubeconfig YAML — pasted, or read from a file the user chose.
    pub yaml: String,
    /// File name to store it under, without extension.
    pub label: String,
    /// Context renames applied before writing.
    #[serde(default)]
    pub renames: BTreeMap<String, String>,
}

fn sanitise(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "imported".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Write an imported kubeconfig, returning its path.
pub fn import(request: &ImportRequest) -> Result<PathBuf, String> {
    let directory = managed_directory().ok_or("no config directory on this platform")?;
    std::fs::create_dir_all(&directory).map_err(|err| err.to_string())?;

    let yaml = if request.renames.is_empty() {
        request.yaml.clone()
    } else {
        k8s_core::kubeconfig::rename_contexts(&request.yaml, &request.renames)
            .map_err(|err| err.to_string())?
    };

    // Cluster and user names are merged across every managed file by name, and
    // two kubeadm clusters both ship `kubernetes` / `kubernetes-admin`. Without
    // this the second import silently inherits the first one's certificate.
    let yaml = k8s_core::kubeconfig::qualify_entries(&yaml, &sanitise(&request.label))
        .map_err(|err| err.to_string())?;

    // Parse before writing: a file that cannot be read back would silently
    // disappear from the cluster list with no explanation.
    k8s_core::kubeconfig::preview(&yaml).map_err(|err| err.to_string())?;

    let mut path = directory.join(format!("{}.yaml", sanitise(&request.label)));
    let mut counter = 2;
    while path.exists() {
        path = directory.join(format!("{}-{counter}.yaml", sanitise(&request.label)));
        counter += 1;
    }

    write_private(&path, &yaml)?;
    Ok(path)
}

/// Kubeconfigs hold credentials, so they are written owner-only.
fn write_private(path: &PathBuf, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|err| err.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Remove an imported kubeconfig. Only files inside the managed directory are
/// eligible, so a crafted path cannot delete something else.
pub fn remove(file: &str) -> Result<(), String> {
    let directory = managed_directory().ok_or("no config directory on this platform")?;
    let path = PathBuf::from(file);

    let canonical_directory = directory.canonicalize().map_err(|err| err.to_string())?;
    let canonical_path = path.canonicalize().map_err(|err| err.to_string())?;
    if !canonical_path.starts_with(&canonical_directory) {
        return Err("that file is not managed by this app".into());
    }

    std::fs::remove_file(&canonical_path).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_become_safe_file_names() {
        assert_eq!(sanitise("Staging Cluster"), "staging-cluster");
        assert_eq!(sanitise("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitise(""), "imported");
        assert_eq!(sanitise("---"), "imported");
    }

    #[test]
    fn conflicts_are_detected_and_given_a_suggestion() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters: [{name: c1, cluster: {server: https://example.test}}]
users: [{name: u1, user: {}}]
contexts: [{name: default, context: {cluster: c1, user: u1}}]
current-context: default
"#;
        let preview = preview(yaml, &["default".to_string()]).unwrap();
        assert_eq!(preview.contexts, vec!["default"]);
        assert_eq!(preview.conflicts, vec!["default"]);
        assert_eq!(
            preview.suggested.get("default").map(String::as_str),
            Some("default-imported")
        );
    }

    #[test]
    fn a_non_conflicting_import_needs_no_rename() {
        let yaml = r#"
apiVersion: v1
kind: Config
clusters: [{name: c1, cluster: {server: https://example.test}}]
users: [{name: u1, user: {}}]
contexts: [{name: staging, context: {cluster: c1, user: u1}}]
"#;
        let preview = preview(yaml, &["production".to_string()]).unwrap();
        assert!(preview.conflicts.is_empty());
        assert!(preview.suggested.is_empty());
    }

    #[test]
    fn a_document_without_contexts_is_rejected() {
        let yaml = "apiVersion: v1\nkind: Config\nclusters: []\n";
        assert!(preview(yaml, &[]).is_err());
    }

    #[test]
    fn nonsense_input_is_rejected_rather_than_stored() {
        assert!(preview("this is not a kubeconfig", &[]).is_err());
    }
}
