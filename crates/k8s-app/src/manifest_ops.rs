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
