//! Adding and removing clusters, shared by every front end.
//!
//! A cluster is added by copying contexts into this app's own folder, never by
//! editing `~/.kube/config`, which kubectl and every other tool share. The
//! system file is only read, and only when the user asks to pick from it.

use std::{collections::BTreeMap, future::Future, path::PathBuf, sync::Arc};

use k8s_core::kubeconfig::{self, ContextEntry};

use crate::{
    clusters::{self, ImportPreview, ImportRequest, ManagedKubeconfig},
    sources::Audited,
    state::AppCore,
};

/// What adding and removing clusters needs of the app: the contexts it knows,
/// its list of managed kubeconfigs, the means to drop a cluster's connection,
/// and the audit log.
pub trait ClusterRegistry: Audited {
    /// Names of the contexts the app already has.
    fn context_names(&self) -> Vec<String>;

    /// Replace the list of managed kubeconfig files and reload.
    fn set_managed(&self, files: Vec<PathBuf>) -> Result<(), String>;

    /// Drop everything tied to a cluster, then its connection.
    fn disconnect(&self, cluster: &str) -> impl Future<Output = ()> + Send;
}

impl ClusterRegistry for AppCore {
    fn context_names(&self) -> Vec<String> {
        self.clusters
            .contexts()
            .into_iter()
            .map(|context| context.name)
            .collect()
    }

    fn set_managed(&self, files: Vec<PathBuf>) -> Result<(), String> {
        self.clusters
            .set_managed(files)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn disconnect(&self, cluster: &str) -> impl Future<Output = ()> + Send {
        AppCore::disconnect(self, cluster)
    }
}

impl<H: ClusterRegistry + ?Sized> ClusterRegistry for Arc<H> {
    fn context_names(&self) -> Vec<String> {
        (**self).context_names()
    }

    fn set_managed(&self, files: Vec<PathBuf>) -> Result<(), String> {
        (**self).set_managed(files)
    }

    fn disconnect(&self, cluster: &str) -> impl Future<Output = ()> + Send {
        (**self).disconnect(cluster)
    }
}

/// The audit log is keyed by cluster, and these changes belong to none.
const AUDIT_SCOPE: &str = "(app)";

/// Context names the app already knows.
fn known(core: &impl ClusterRegistry) -> Vec<String> {
    core.context_names()
}

/// What importing a kubeconfig would add, and which names it would clash with.
pub fn preview(core: &impl ClusterRegistry, yaml: &str) -> Result<ImportPreview, String> {
    clusters::preview(yaml, &known(core))
}

/// The kubeconfigs this app manages, with the contexts each holds.
pub fn managed() -> Vec<ManagedKubeconfig> {
    clusters::list()
}

/// Contexts in the system kubeconfig, for picking from. Reading is not adding:
/// nothing connects until the user picks one and it is copied in.
pub fn system_contexts() -> Vec<ContextEntry> {
    kubeconfig::system_contexts()
}

/// The chosen contexts of the system kubeconfig, with the clusters and users
/// they reference, as a standalone document.
pub fn extract_system(wanted: &[String]) -> Result<String, String> {
    let source = kubeconfig::read_system().map_err(|e| e.to_string())?;
    kubeconfig::extract(&source, wanted).map_err(|e| e.to_string())
}

/// Add a kubeconfig, applying renames to colliding names. Returns the context
/// names it added.
pub fn add(core: &impl ClusterRegistry, request: &ImportRequest) -> Result<Vec<String>, String> {
    let result = add_inner(core, request);
    core.audit(AUDIT_SCOPE, "addCluster", &request.label, &result);
    result
}

fn add_inner(core: &impl ClusterRegistry, request: &ImportRequest) -> Result<Vec<String>, String> {
    let contexts = kubeconfig::preview(&request.yaml).map_err(|e| e.to_string())?;
    let added = resolve_names(&contexts, &request.renames, &known(core))?;

    clusters::import(request)?;
    core.set_managed(clusters::managed_files())?;
    Ok(added)
}

/// Names the contexts will have once renamed, refusing a blank name or one that
/// is taken. A clash left in place would give two tiles one name and no way to
/// tell which cluster a click reaches.
fn resolve_names(
    contexts: &[String],
    renames: &BTreeMap<String, String>,
    existing: &[String],
) -> Result<Vec<String>, String> {
    let mut added: Vec<String> = Vec::with_capacity(contexts.len());
    for name in contexts {
        let renamed = renames
            .get(name)
            .map_or(name.as_str(), |value| value.trim());
        if renamed.is_empty() {
            return Err(format!("Give `{name}` a name."));
        }
        if existing.iter().any(|known| known == renamed) || added.iter().any(|a| a == renamed) {
            return Err(format!(
                "A cluster named `{renamed}` already exists. Rename it."
            ));
        }
        added.push(renamed.to_string());
    }
    Ok(added)
}

/// Remove a managed kubeconfig and every cluster it held: their connections
/// are dropped first, so nothing keeps running against a cluster that is gone.
pub async fn remove(core: &impl ClusterRegistry, file: &str) -> Result<(), String> {
    let contexts = clusters::list()
        .into_iter()
        .find(|managed| managed.file == file)
        .map(|managed| managed.contexts)
        .unwrap_or_default();

    let result = async {
        for context in &contexts {
            core.disconnect(context).await;
        }
        clusters::remove(file)?;
        core.set_managed(clusters::managed_files())
    }
    .await;
    core.audit(AUDIT_SCOPE, "removeCluster", &contexts.join(", "), &result);
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use std::{fmt::Display, path::PathBuf};

    use super::{ClusterRegistry, preview, resolve_names};
    use crate::sources::Audited;

    /// An app that already knows two contexts.
    struct Known;

    impl Audited for Known {
        fn audit<T, E: Display>(&self, _: &str, _: &str, _: &str, _: &Result<T, E>) {}
    }

    impl ClusterRegistry for Known {
        fn context_names(&self) -> Vec<String> {
            vec!["prod".to_string(), "staging".to_string()]
        }

        fn set_managed(&self, _: Vec<PathBuf>) -> Result<(), String> {
            Ok(())
        }

        async fn disconnect(&self, _: &str) {}
    }

    #[test]
    fn importing_a_context_the_app_already_has_is_a_conflict_with_a_new_name_suggested() {
        let yaml = "apiVersion: v1\nkind: Config\ncurrent-context: prod\n\
            clusters:\n- name: c\n  cluster: {server: 'https://127.0.0.1:6443'}\n\
            users:\n- name: u\n  user: {token: t}\n\
            contexts:\n- name: prod\n  context: {cluster: c, user: u}\n\
            - name: dev\n  context: {cluster: c, user: u}\n";
        let shown = preview(&Known, yaml).expect("a preview");
        assert_eq!(shown.conflicts, ["prod"]);
        assert_eq!(
            shown.suggested.get("prod").map(String::as_str),
            Some("prod-imported")
        );
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_new_name_is_kept_and_a_clash_must_be_renamed() {
        let none = BTreeMap::new();
        assert_eq!(
            resolve_names(&names(&["staging"]), &none, &names(&["prod"])),
            Ok(names(&["staging"]))
        );
        assert!(resolve_names(&names(&["prod"]), &none, &names(&["prod"])).is_err());

        let renames = BTreeMap::from([("prod".to_string(), " prod-2 ".to_string())]);
        assert_eq!(
            resolve_names(&names(&["prod"]), &renames, &names(&["prod"])),
            Ok(names(&["prod-2"])),
            "a rename is trimmed and then accepted"
        );
    }

    #[test]
    fn two_contexts_cannot_end_up_with_one_name() {
        let renames = BTreeMap::from([("b".to_string(), "a".to_string())]);
        assert!(resolve_names(&names(&["a", "b"]), &renames, &[]).is_err());
        let blank = BTreeMap::from([("a".to_string(), "  ".to_string())]);
        assert!(resolve_names(&names(&["a"]), &blank, &[]).is_err());
    }
}
