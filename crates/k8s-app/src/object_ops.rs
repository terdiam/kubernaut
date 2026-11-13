//! Operations on a single object, shared by every front end.
//!
//! Like [`crate::helm_ops`], each one refuses a protected cluster and leaves
//! an audit entry whatever the outcome. Confirming with the user is the front
//! end's job: by the time these run, they have said yes.

use k8s_ops::actions::{self, DeleteRequest, TargetRef};

use crate::state::AppCore;

fn label(target: &TargetRef) -> String {
    match &target.namespace {
        Some(namespace) => format!("{}/{namespace}/{}", target.resource, target.name),
        None => format!("{}/{}", target.resource, target.name),
    }
}

/// Delete an object with the default (background) propagation.
pub async fn delete(core: &AppCore, cluster: &str, target: TargetRef) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let audit_label = label(&target);
    let request = DeleteRequest {
        confirmation: target.name.clone(),
        target,
        propagation: None,
        grace_period_seconds: None,
    };
    let result = actions::delete(&handle, &request).await;
    core.record_audit(cluster, "delete", &audit_label, &result);
    result.map_err(|e| e.to_string())
}

/// Roll a workload's pods, as `kubectl rollout restart` does.
pub async fn restart(core: &AppCore, cluster: &str, target: TargetRef) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = actions::restart(&handle, &target).await;
    core.record_audit(cluster, "restart", &label(&target), &result);
    result.map_err(|e| e.to_string())
}

/// Mark a node (un)schedulable.
pub async fn set_cordoned(
    core: &AppCore,
    cluster: &str,
    node: &str,
    cordoned: bool,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = actions::set_cordoned(&handle, node, cordoned).await;
    let action = if cordoned { "cordon" } else { "uncordon" };
    core.record_audit(cluster, action, &format!("nodes/{node}"), &result);
    result.map_err(|e| e.to_string())
}
