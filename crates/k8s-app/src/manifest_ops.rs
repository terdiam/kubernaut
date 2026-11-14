//! Creating and importing resources from manifests, shared by every front end.
//!
//! Planning reads the cluster and never writes, so a protected context may
//! still be planned against. Applying is a write: it is refused for a
//! protected context and audited whatever the outcome.

use std::path::Path;

use k8s_ops::manifest::{self, DocResult, ManifestPlan};

use crate::state::AppCore;

/// What applying a manifest would do to each document, without doing it.
pub async fn plan(
    core: &AppCore,
    cluster: &str,
    yaml: &str,
    namespace: Option<&str>,
    force: bool,
) -> Result<ManifestPlan, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    manifest::plan(&handle, yaml, namespace, force)
        .await
        .map_err(|e| e.to_string())
}

/// Apply every document, reporting each one. A document that fails does not
/// stop the rest, because stopping halfway leaves a worse state than finishing.
pub async fn apply(
    core: &AppCore,
    cluster: &str,
    yaml: &str,
    namespace: Option<&str>,
    force: bool,
) -> Result<Vec<DocResult>, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = manifest::apply(&handle, yaml, namespace, force)
        .await
        .map_err(|e| e.to_string());

    let outcome = match &result {
        Ok(results) => {
            let failed = results
                .iter()
                .filter(|r| r.status == "error" || r.status == "conflict")
                .count();
            if failed == 0 {
                Ok(())
            } else {
                Err(format!(
                    "{failed} of {} documents were not applied",
                    results.len()
                ))
            }
        }
        Err(err) => Err(err.clone()),
    };
    let target = match &result {
        Ok(results) => format!("{} documents", results.len()),
        Err(_) => "manifest".to_string(),
    };
    core.record_audit(cluster, "applyManifest", &target, &outcome);
    result
}

/// Build a local kustomize directory into plain YAML. Touches no cluster.
pub async fn kustomize_build(core: &AppCore, directory: &Path) -> Result<String, String> {
    let kustomize = core.kustomize().map_err(|e| e.to_string())?;
    kustomize.build(directory).await.map_err(|e| e.to_string())
}

/// Suggestions for a form field that refers to another object: the services in
/// a namespace, the storage classes, and so on. `param` narrows a dependent
/// list, such as the ports of the service already chosen.
///
/// A cluster that cannot answer is not an error to the form, which falls back
/// to the field being typed, so failures give an empty list.
pub async fn lookup_options(
    core: &AppCore,
    cluster: &str,
    source: &str,
    namespace: Option<&str>,
    param: Option<&str>,
) -> Vec<k8s_ops::lookup::LookupOption> {
    let Ok(handle) = core.clusters.require(cluster) else {
        return Vec::new();
    };
    k8s_ops::lookup::lookup(&handle, source, namespace, param)
        .await
        .unwrap_or_default()
}
