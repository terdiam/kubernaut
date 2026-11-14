//! Operations on a single object, shared by every front end.
//!
//! Like [`crate::helm_ops`], each one refuses a protected cluster and leaves
//! an audit entry whatever the outcome. Confirming with the user is the front
//! end's job: by the time these run, they have said yes.

use std::collections::BTreeMap;

use k8s_ops::{
    actions::{
        self, BulkOutcome, DeleteRequest, DrainOptions, DrainReport, ExportResult, TargetRef,
    },
    node_edit::{self, NodeTags, Taint},
};

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

/// What draining a node would do, without cordoning it or evicting anything.
/// Reading only, so a protected cluster may be previewed.
pub async fn drain_preview(
    core: &AppCore,
    cluster: &str,
    node: &str,
    delete_standalone_pods: bool,
) -> Result<DrainReport, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let options = DrainOptions {
        confirmation: node.to_string(),
        delete_standalone_pods,
        dry_run: true,
    };
    actions::drain(&handle, node, &options)
        .await
        .map_err(|e| e.to_string())
}

/// Cordon a node and evict its pods through the eviction API, which respects
/// PodDisruptionBudgets. A pod the budget refuses is reported, not forced.
pub async fn drain(
    core: &AppCore,
    cluster: &str,
    node: &str,
    delete_standalone_pods: bool,
) -> Result<DrainReport, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let options = DrainOptions {
        confirmation: node.to_string(),
        delete_standalone_pods,
        dry_run: false,
    };
    let result = actions::drain(&handle, node, &options).await;
    // Evictions the apiserver refused do not make the drain a failure of this
    // call, but they are worth a line in the log.
    let outcome = match &result {
        Ok(report) if report.blocked.is_empty() => Ok(()),
        Ok(report) => Err(format!(
            "{} pods could not be evicted",
            report.blocked.len()
        )),
        Err(err) => Err(err.to_string()),
    };
    core.record_audit(cluster, "drain", &format!("nodes/{node}"), &outcome);
    result.map_err(|e| e.to_string())
}

/// A node's labels and taints, to edit.
pub async fn read_node_tags(core: &AppCore, cluster: &str, node: &str) -> Result<NodeTags, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    node_edit::read(&handle, node)
        .await
        .map_err(|e| e.to_string())
}

/// Save a node's labels and taints, refusing if the node changed since `before`
/// was read.
pub async fn save_node_tags(
    core: &AppCore,
    cluster: &str,
    node: &str,
    before: &NodeTags,
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = node_edit::apply(&handle, node, before, labels, taints).await;
    core.record_audit(cluster, "editNodeTags", &format!("nodes/{node}"), &result);
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

// -------------------------------------------------------------------- bulk

/// How many of a bulk operation's objects did not go through.
pub fn failed(outcomes: &[BulkOutcome]) -> usize {
    outcomes.iter().filter(|outcome| !outcome.ok).count()
}

/// The audit line for a bulk change: a failure to do all of it is a failure,
/// so it is not recorded as "ok".
fn bulk_result(outcomes: &[BulkOutcome]) -> Result<(), String> {
    match failed(outcomes) {
        0 => Ok(()),
        n => Err(format!("{n} of {} were not done", outcomes.len())),
    }
}

fn bulk_label(targets: &[TargetRef]) -> String {
    match targets.first() {
        Some(first) => format!("{} × {}", targets.len(), first.resource),
        None => "nothing".to_string(),
    }
}

/// Delete several objects. One that fails does not stop the rest, and each
/// one's outcome is returned.
pub async fn delete_many(
    core: &AppCore,
    cluster: &str,
    targets: Vec<TargetRef>,
) -> Result<Vec<BulkOutcome>, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    // The count is the guard the backend asks for. The front end has already
    // shown what is about to go and asked, so it is supplied here.
    let confirmation = targets.len().to_string();
    let result = actions::delete_many(&handle, &targets, &confirmation).await;
    let audited = match &result {
        Ok(outcomes) => bulk_result(outcomes),
        Err(err) => Err(err.to_string()),
    };
    core.record_audit(cluster, "bulkDelete", &bulk_label(&targets), &audited);
    result.map_err(|e| e.to_string())
}

/// Roll several workloads, as `kubectl rollout restart` does.
pub async fn restart_many(
    core: &AppCore,
    cluster: &str,
    targets: Vec<TargetRef>,
) -> Result<Vec<BulkOutcome>, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let result = actions::restart_many(&handle, &targets).await;
    let audited = match &result {
        Ok(outcomes) => bulk_result(outcomes),
        Err(err) => Err(err.to_string()),
    };
    core.record_audit(cluster, "bulkRestart", &bulk_label(&targets), &audited);
    result.map_err(|e| e.to_string())
}

/// Write the objects to a zip, one YAML file per object. It only reads the
/// cluster, so a protected one may be exported.
pub async fn export_archive(
    core: &AppCore,
    cluster: &str,
    targets: Vec<TargetRef>,
    path: std::path::PathBuf,
) -> Result<ExportResult, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    actions::export_archive(&handle, &targets, &path)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{BulkOutcome, bulk_label, bulk_result, failed};
    use k8s_ops::actions::TargetRef;

    fn outcome(ok: bool) -> BulkOutcome {
        BulkOutcome {
            resource: "core/v1/pods".into(),
            namespace: Some("shop".into()),
            name: "a".into(),
            ok,
            error: (!ok).then(|| "forbidden".to_string()),
        }
    }

    #[test]
    fn a_bulk_change_is_ok_only_when_every_object_went_through() {
        assert_eq!(failed(&[outcome(true), outcome(false), outcome(false)]), 2);
        assert!(bulk_result(&[outcome(true), outcome(true)]).is_ok());
        assert_eq!(
            bulk_result(&[outcome(true), outcome(false)]),
            Err("1 of 2 were not done".to_string())
        );
    }

    #[test]
    fn the_audit_label_says_how_many_of_what() {
        let target = |name: &str| TargetRef {
            resource: "core/v1/pods".into(),
            namespace: None,
            name: name.into(),
        };
        assert_eq!(bulk_label(&[target("a"), target("b")]), "2 × core/v1/pods");
        assert_eq!(bulk_label(&[]), "nothing");
    }
}
