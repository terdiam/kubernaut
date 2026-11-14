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

/// Ask a GitOps controller to reconcile an object now. Writes an annotation.
pub async fn gitops_reconcile(
    core: &AppCore,
    cluster: &str,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = k8s_ops::gitops::reconcile(&handle, resource, namespace, name).await;
    let target = match namespace {
        Some(namespace) => format!("{resource}/{namespace}/{name}"),
        None => format!("{resource}/{name}"),
    };
    core.record_audit(cluster, "gitopsReconcile", &target, &result);
    result.map_err(|e| e.to_string())
}

/// Pause or resume a Flux object's reconciliation.
pub async fn gitops_set_suspended(
    core: &AppCore,
    cluster: &str,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
    suspended: bool,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result =
        k8s_ops::gitops::set_suspended(&handle, resource, namespace, name, suspended).await;
    let action = if suspended {
        "gitopsSuspend"
    } else {
        "gitopsResume"
    };
    let target = match namespace {
        Some(namespace) => format!("{resource}/{namespace}/{name}"),
        None => format!("{resource}/{name}"),
    };
    core.record_audit(cluster, action, &target, &result);
    result.map_err(|e| e.to_string())
}

/// What the installed GitOps controllers manage, optionally in one namespace.
pub async fn gitops_survey(
    core: &AppCore,
    cluster: &str,
    namespace: Option<&str>,
) -> Result<k8s_ops::gitops::GitOpsSummary, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    k8s_ops::gitops::survey(&handle, namespace)
        .await
        .map_err(|e| e.to_string())
}
