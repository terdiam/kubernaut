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
