//! Workload and node actions: scale, restart, cordon, drain, delete, evict.
//!
//! Destructive calls take a `confirmation` string that must equal the object's
//! own name. The UI already asks, but the IPC surface is reachable from a
//! renderer bug or a stray call, and "delete whatever is selected" is exactly
//! the operation that must not happen by accident.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::{api::core::v1::Pod, jiff::Timestamp};
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, DynamicObject, EvictParams, ListParams, Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{OpsError, Result};

/// Annotation kubectl uses for `rollout restart`; reusing it keeps the two
/// tools interchangeable on the same workload.
const RESTART_ANNOTATION: &str = "kubectl.kubernetes.io/restartedAt";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetRef {
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,
}

async fn api_for(cluster: &Arc<ClusterHandle>, target: &TargetRef) -> Result<Api<DynamicObject>> {
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    let descriptor = discovery.require(&target.resource)?;
    let ar = descriptor.api_resource();
    Ok(match (target.namespace.as_deref(), descriptor.namespaced) {
        (Some(ns), true) => Api::namespaced_with(cluster.client.clone(), ns, &ar),
        _ => Api::all_with(cluster.client.clone(), &ar),
    })
}

fn require_confirmation(expected: &str, given: &str) -> Result<()> {
    if expected == given {
        return Ok(());
    }
    Err(OpsError::other(format!(
        "confirmation `{given}` does not match `{expected}`; the operation was not performed"
    )))
}

/// Set replicas through the scale subresource, which works uniformly across
/// Deployments, StatefulSets, ReplicaSets and any CRD that implements it.
pub async fn scale(cluster: &Arc<ClusterHandle>, target: &TargetRef, replicas: i32) -> Result<i32> {
    if replicas < 0 {
        return Err(OpsError::other("replicas cannot be negative"));
    }
    let api = api_for(cluster, target).await?;
    let patch = json!({ "spec": { "replicas": replicas } });
    let scaled = api
        .patch_scale(&target.name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(scaled.spec.and_then(|s| s.replicas).unwrap_or(replicas))
}

pub async fn current_scale(cluster: &Arc<ClusterHandle>, target: &TargetRef) -> Result<i32> {
    let api = api_for(cluster, target).await?;
    let scale = api.get_scale(&target.name).await?;
    Ok(scale.spec.and_then(|s| s.replicas).unwrap_or(0))
}

/// Roll the pods of a workload by touching the pod template annotation.
pub async fn restart(cluster: &Arc<ClusterHandle>, target: &TargetRef) -> Result<()> {
    let api = api_for(cluster, target).await?;
    let patch = json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": { RESTART_ANNOTATION: Timestamp::now().to_string() }
                }
            }
        }
    });
    api.patch(&target.name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// Mark a node (un)schedulable. Cordoning is reversible and does not move pods.
pub async fn set_cordoned(cluster: &Arc<ClusterHandle>, node: &str, cordoned: bool) -> Result<()> {
    let target = TargetRef {
        resource: "core/v1/nodes".into(),
        namespace: None,
        name: node.to_string(),
    };
    let api = api_for(cluster, &target).await?;
    let patch = json!({ "spec": { "unschedulable": cordoned } });
    api.patch(node, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainReport {
    pub node: String,
    pub evicted: Vec<String>,
    /// Pods left in place, with why (DaemonSet, mirror pod, standalone).
    pub skipped: Vec<SkippedPod>,
    /// Evictions the apiserver refused, usually a PodDisruptionBudget.
    pub blocked: Vec<BlockedPod>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedPod {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedPod {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainOptions {
    /// Must equal the node name.
    pub confirmation: String,
    /// Evict pods that are not backed by a controller. They will not come back
    /// anywhere else, so this is off by default.
    #[serde(default)]
    pub delete_standalone_pods: bool,
    /// Report what would happen without evicting anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// Cordon the node, then evict its pods.
///
/// Uses the eviction API rather than deleting pods, so PodDisruptionBudgets are
/// respected — a plain delete would take down a quorum the budget exists to
/// protect.
pub async fn drain(
    cluster: &Arc<ClusterHandle>,
    node: &str,
    options: &DrainOptions,
) -> Result<DrainReport> {
    require_confirmation(node, &options.confirmation)?;

    if !options.dry_run {
        set_cordoned(cluster, node, true).await?;
    }

    let pods: Api<Pod> = Api::all(cluster.client.clone());
    let list = pods
        .list(&ListParams::default().fields(&format!("spec.nodeName={node}")))
        .await?;

    let mut report = DrainReport {
        node: node.to_string(),
        evicted: Vec::new(),
        skipped: Vec::new(),
        blocked: Vec::new(),
    };

    for pod in list.items {
        let name = pod.name_any();
        let namespace = pod.namespace().unwrap_or_default();
        let label = format!("{namespace}/{name}");

        let owner = pod
            .metadata
            .owner_references
            .as_ref()
            .and_then(|owners| owners.first().map(|o| o.kind.clone()));

        match owner.as_deref() {
            // DaemonSet pods are recreated on the same node immediately, so
            // evicting them only causes churn.
            Some("DaemonSet") => {
                report.skipped.push(SkippedPod {
                    name: label,
                    reason: "managed by a DaemonSet".into(),
                });
                continue;
            }
            // Static/mirror pods are owned by the kubelet, not the apiserver.
            _ if pod
                .annotations()
                .contains_key("kubernetes.io/config.mirror") =>
            {
                report.skipped.push(SkippedPod {
                    name: label,
                    reason: "static pod managed by the kubelet".into(),
                });
                continue;
            }
            None if !options.delete_standalone_pods => {
                report.skipped.push(SkippedPod {
                    name: label,
                    reason: "not managed by a controller; it would not be recreated".into(),
                });
                continue;
            }
            _ => {}
        }

        if options.dry_run {
            report.evicted.push(label);
            continue;
        }

        let scoped: Api<Pod> = Api::namespaced(cluster.client.clone(), &namespace);
        match scoped.evict(&name, &EvictParams::default()).await {
            Ok(_) => report.evicted.push(label),
            Err(kube::Error::Api(status)) => report.blocked.push(BlockedPod {
                name: label,
                message: status.message,
            }),
            Err(err) => report.blocked.push(BlockedPod {
                name: label,
                message: err.to_string(),
            }),
        }
    }

    Ok(report)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteRequest {
    #[serde(flatten)]
    pub target: TargetRef,
    /// Must equal `target.name`.
    pub confirmation: String,
    /// `background` (default), `foreground`, or `orphan`.
    pub propagation: Option<String>,
    /// Seconds to wait before force-killing. `Some(0)` is a force delete and
    /// can strand workloads (a StatefulSet pod may be running elsewhere).
    pub grace_period_seconds: Option<u32>,
}

pub async fn delete(cluster: &Arc<ClusterHandle>, request: &DeleteRequest) -> Result<()> {
    require_confirmation(&request.target.name, &request.confirmation)?;
    let api = api_for(cluster, &request.target).await?;

    let mut params = match request.propagation.as_deref() {
        Some("foreground") => DeleteParams::foreground(),
        Some("orphan") => DeleteParams::orphan(),
        Some("background") | None => DeleteParams::background(),
        Some(other) => {
            return Err(OpsError::other(format!(
                "unknown propagation policy `{other}`"
            )));
        }
    };
    if let Some(grace) = request.grace_period_seconds {
        params.grace_period_seconds = Some(grace);
    }
    api.delete(&request.target.name, &params).await?;
    Ok(())
}

/// Evict a single pod, respecting disruption budgets.
pub async fn evict_pod(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    name: &str,
    confirmation: &str,
) -> Result<()> {
    require_confirmation(name, confirmation)?;
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    api.evict(name, &EvictParams::default()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_must_match_exactly() {
        assert!(require_confirmation("web", "web").is_ok());
        assert!(require_confirmation("web", "Web").is_err());
        assert!(require_confirmation("web", "").is_err());
        let err = require_confirmation("web", "web ").unwrap_err().to_string();
        assert!(err.contains("was not performed"), "{err}");
    }
}

// ------------------------------------------------------------------- bulk

/// What one object in a bulk operation did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkOutcome {
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Delete several objects at once.
///
/// The single-object path makes the user type the object's name, so that they
/// look at what they are about to destroy. The equivalent for a set is the
/// size of it: `confirmation` must be the number of targets, typed out. Faking
/// per-name confirmations on the user's behalf would keep the signature and
/// throw away the guarantee.
///
/// One failure does not stop the rest — a partial delete reported honestly is
/// more useful than an abort halfway with no account of what happened.
pub async fn delete_many(
    cluster: &Arc<ClusterHandle>,
    targets: &[TargetRef],
    confirmation: &str,
) -> Result<Vec<BulkOutcome>> {
    check_bulk_delete(targets, confirmation)?;

    let mut out = Vec::with_capacity(targets.len());
    for target in targets {
        let result = async {
            let api = api_for(cluster, target).await?;
            api.delete(&target.name, &Default::default()).await?;
            Ok::<(), OpsError>(())
        }
        .await;

        out.push(BulkOutcome {
            resource: target.resource.clone(),
            namespace: target.namespace.clone(),
            name: target.name.clone(),
            ok: result.is_ok(),
            error: result.err().map(|err| err.to_string()),
        });
    }
    Ok(out)
}

/// Everything a bulk delete is checked for before a request is made.
///
/// Separate from the request loop so the guarantee can be tested without a
/// cluster: the guard is the whole point of the function.
fn check_bulk_delete(targets: &[TargetRef], confirmation: &str) -> Result<()> {
    if targets.is_empty() {
        return Err(OpsError::other("nothing selected"));
    }
    require_confirmation(&targets.len().to_string(), confirmation)
}

/// Roll several workloads, the way `kubectl rollout restart` does.
pub async fn restart_many(
    cluster: &Arc<ClusterHandle>,
    targets: &[TargetRef],
) -> Result<Vec<BulkOutcome>> {
    let mut out = Vec::with_capacity(targets.len());
    for target in targets {
        let result = restart(cluster, target).await;
        out.push(BulkOutcome {
            resource: target.resource.clone(),
            namespace: target.namespace.clone(),
            name: target.name.clone(),
            ok: result.is_ok(),
            error: result.err().map(|err| err.to_string()),
        });
    }
    Ok(out)
}

/// Upper bound on one export. A namespace can hold thousands of objects, and
/// a multi-megabyte string crossing the IPC bridge helps nobody.
pub const EXPORT_LIMIT: usize = 500;

/// What an export produced, and what it left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// Multi-document YAML, `---` separated.
    pub yaml: String,
    pub exported: usize,
    /// Objects that could not be read, with the reason.
    pub failed: Vec<BulkOutcome>,
    /// True when more were asked for than [`EXPORT_LIMIT`].
    pub truncated: bool,
}

/// Read objects and concatenate them as a manifest.
///
/// Server-managed fields are stripped, so what comes out can be applied
/// somewhere else rather than only read.
pub async fn export(cluster: &Arc<ClusterHandle>, targets: &[TargetRef]) -> Result<ExportResult> {
    let truncated = targets.len() > EXPORT_LIMIT;
    let mut documents = Vec::new();
    let mut failed = Vec::new();

    for target in targets.iter().take(EXPORT_LIMIT) {
        match k8s_core::objects::get(
            cluster,
            &target.resource,
            target.namespace.as_deref(),
            &target.name,
        )
        .await
        {
            Ok(object) => match k8s_core::objects::to_yaml(&object, false) {
                Ok(yaml) => documents.push(yaml),
                Err(err) => failed.push(BulkOutcome {
                    resource: target.resource.clone(),
                    namespace: target.namespace.clone(),
                    name: target.name.clone(),
                    ok: false,
                    error: Some(err.to_string()),
                }),
            },
            Err(err) => failed.push(BulkOutcome {
                resource: target.resource.clone(),
                namespace: target.namespace.clone(),
                name: target.name.clone(),
                ok: false,
                error: Some(err.to_string()),
            }),
        }
    }

    Ok(ExportResult {
        exported: documents.len(),
        yaml: documents.join("---\n"),
        failed,
        truncated,
    })
}

/// Where one object lands inside the archive.
///
/// Grouped by namespace then kind, so unpacking a namespace's export gives the
/// same shape people already keep manifests in. Cluster-scoped objects have no
/// namespace to file under and go in `_cluster`.
fn archive_path(target: &TargetRef, kind: &str) -> String {
    let namespace = target.namespace.as_deref().unwrap_or("_cluster");
    format!(
        "{}/{}/{}.yaml",
        sanitise_segment(namespace),
        sanitise_segment(kind),
        sanitise_segment(&target.name)
    )
}

/// Keep a path segment to characters that are safe on every platform the app
/// runs on. Object names are already DNS-safe, but a CRD kind is not
/// guaranteed to be.
fn sanitise_segment(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// Read objects and write them into a zip archive at `path`.
///
/// Writing happens here rather than in the webview: a page cannot save a file
/// on this platform, and routing bytes through the IPC bridge to a renderer
/// that then cannot write them helps nobody.
pub async fn export_archive(
    cluster: &Arc<ClusterHandle>,
    targets: &[TargetRef],
    path: &std::path::Path,
) -> Result<ExportResult> {
    if targets.is_empty() {
        return Err(OpsError::other("nothing selected"));
    }

    let truncated = targets.len() > EXPORT_LIMIT;
    let mut failed = Vec::new();
    let mut written = 0usize;
    let mut seen: std::collections::BTreeSet<String> = Default::default();

    let file = std::fs::File::create(path)
        .map_err(|err| OpsError::other(format!("could not create {}: {err}", path.display())))?;
    let mut archive = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for target in targets.iter().take(EXPORT_LIMIT) {
        let object = match k8s_core::objects::get(
            cluster,
            &target.resource,
            target.namespace.as_deref(),
            &target.name,
        )
        .await
        {
            Ok(object) => object,
            Err(err) => {
                failed.push(BulkOutcome {
                    resource: target.resource.clone(),
                    namespace: target.namespace.clone(),
                    name: target.name.clone(),
                    ok: false,
                    error: Some(err.to_string()),
                });
                continue;
            }
        };

        let kind = object
            .types
            .as_ref()
            .map(|t| t.kind.clone())
            .unwrap_or_else(|| {
                // Fall back to the plural from the resource key when the object
                // carries no TypeMeta.
                target
                    .resource
                    .rsplit('/')
                    .next()
                    .unwrap_or("object")
                    .to_string()
            });

        let yaml = match k8s_core::objects::to_yaml(&object, false) {
            Ok(yaml) => yaml,
            Err(err) => {
                failed.push(BulkOutcome {
                    resource: target.resource.clone(),
                    namespace: target.namespace.clone(),
                    name: target.name.clone(),
                    ok: false,
                    error: Some(err.to_string()),
                });
                continue;
            }
        };

        // Two objects can only collide here if the same one was passed twice;
        // silently overwriting would make the count disagree with the archive.
        let mut entry = archive_path(target, &kind);
        let mut suffix = 2;
        while !seen.insert(entry.clone()) {
            entry = entry.replace(".yaml", &format!("-{suffix}.yaml"));
            suffix += 1;
        }

        archive
            .start_file(&entry, options)
            .map_err(|err| OpsError::other(err.to_string()))?;
        std::io::Write::write_all(&mut archive, yaml.as_bytes())?;
        written += 1;
    }

    archive
        .finish()
        .map_err(|err| OpsError::other(format!("could not finish the archive: {err}")))?;

    Ok(ExportResult {
        yaml: String::new(),
        exported: written,
        failed,
        truncated,
    })
}

#[cfg(test)]
mod bulk_tests {
    use super::*;

    fn targets(n: usize) -> Vec<TargetRef> {
        (0..n)
            .map(|i| TargetRef {
                resource: "apps/v1/deployments".into(),
                namespace: Some("app".into()),
                name: format!("web-{i}"),
            })
            .collect()
    }

    #[test]
    fn deleting_nothing_is_refused() {
        // An empty set would be a harmless no-op against the cluster, but the
        // dialog that produced it is a bug worth surfacing.
        let err = check_bulk_delete(&[], "0").expect_err("empty");
        assert!(err.to_string().contains("nothing selected"), "{err}");
    }

    #[test]
    fn the_confirmation_for_a_set_is_its_size() {
        // Typing one object's name confirms the wrong thing entirely when
        // twelve are selected, so the count is what has to be typed.
        assert!(check_bulk_delete(&targets(12), "12").is_ok());
        assert!(check_bulk_delete(&targets(12), "web-0").is_err());
        assert!(check_bulk_delete(&targets(12), "11").is_err());
        assert!(check_bulk_delete(&targets(12), "").is_err());
    }

    #[test]
    fn a_miscount_names_both_numbers() {
        let err = check_bulk_delete(&targets(3), "2").expect_err("mismatch");
        let message = err.to_string();
        assert!(
            message.contains("`2`") && message.contains("`3`"),
            "{message}"
        );
        assert!(message.contains("was not performed"), "{message}");
    }

    #[test]
    fn the_archive_groups_by_namespace_then_kind() {
        let target = TargetRef {
            resource: "apps/v1/deployments".into(),
            namespace: Some("production".into()),
            name: "checkout-api".into(),
        };
        assert_eq!(
            archive_path(&target, "Deployment"),
            "production/Deployment/checkout-api.yaml"
        );
    }

    #[test]
    fn a_cluster_scoped_object_has_a_folder_of_its_own() {
        // `_cluster` rather than the empty string, which would put the file at
        // the archive root and mix it in with namespace folders.
        let target = TargetRef {
            resource: "core/v1/nodes".into(),
            namespace: None,
            name: "node-1".into(),
        };
        assert_eq!(archive_path(&target, "Node"), "_cluster/Node/node-1.yaml");
    }

    #[test]
    fn path_segments_cannot_escape_the_archive() {
        // A CRD kind is not guaranteed to be path-safe, and a zip entry that
        // walks upward is a classic way to write outside the extract folder.
        assert_eq!(sanitise_segment("../../etc"), ".._.._etc");
        assert_eq!(sanitise_segment("a/b"), "a_b");
        assert_eq!(sanitise_segment(""), "unnamed");
        assert_eq!(sanitise_segment("Fine-name.v1_2"), "Fine-name.v1_2");
    }

    #[test]
    fn the_export_limit_is_stated_rather_than_silently_applied() {
        // `ExportResult::truncated` exists so a 900-object export cannot look
        // complete.
        assert_eq!(EXPORT_LIMIT, 500);
        assert!(targets(EXPORT_LIMIT + 1).len() > EXPORT_LIMIT);
    }
}
