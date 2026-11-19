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
    apply::{self, ApplyOutcome, EditRequest},
    error::OpsError,
    node_edit::{self, NodeTags, Taint},
    node_pods::{self, PodLimit},
    related::{self, EventRow, Related},
};

use crate::sources::WriteHost;

fn label(target: &TargetRef) -> String {
    match &target.namespace {
        Some(namespace) => format!("{}/{namespace}/{}", target.resource, target.name),
        None => format!("{}/{}", target.resource, target.name),
    }
}

/// Apply an object edited as YAML. A protected cluster refuses it, and every
/// attempt is audited, including the one the cluster turned away because another
/// field manager owns what was changed: that changed nothing, and the entry says so.
pub async fn apply_edit(
    core: &impl WriteHost,
    cluster: &str,
    request: EditRequest,
) -> Result<ApplyOutcome, String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let target = label(&TargetRef {
        resource: request.resource.clone(),
        namespace: request.namespace.clone(),
        name: request.name.clone(),
    });
    let result = apply::apply(&handle, &request).await;
    let action = if request.force {
        "editObject (forced)"
    } else {
        "editObject"
    };
    core.audit(cluster, action, &target, &apply_audit(&result));
    result.map_err(|e| e.to_string())
}

/// What the audit log should say about an apply. Only `Applied` changed the
/// cluster; a field-manager conflict came back as a normal answer but did not.
fn apply_audit(result: &Result<ApplyOutcome, OpsError>) -> Result<(), String> {
    match result {
        Ok(ApplyOutcome::Applied { .. }) => Ok(()),
        Ok(ApplyOutcome::Conflict { conflicts }) => Err(format!(
            "refused: {} field(s) are owned by another manager",
            conflicts.len()
        )),
        Err(err) => Err(err.to_string()),
    }
}

/// Delete an object with the default (background) propagation.
pub async fn delete(core: &impl WriteHost, cluster: &str, target: TargetRef) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let audit_label = label(&target);
    let request = DeleteRequest {
        confirmation: target.name.clone(),
        target,
        propagation: None,
        grace_period_seconds: None,
    };
    let result = actions::delete(&handle, &request).await;
    core.audit(cluster, "delete", &audit_label, &result);
    result.map_err(|e| e.to_string())
}

/// Roll a workload's pods, as `kubectl rollout restart` does.
pub async fn restart(
    core: &impl WriteHost,
    cluster: &str,
    target: TargetRef,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::restart(&handle, &target).await;
    core.audit(cluster, "restart", &label(&target), &result);
    result.map_err(|e| e.to_string())
}

/// Replicas as the scale subresource reports them, for the Scale dialog.
/// Reading only, so a protected cluster may be asked.
pub async fn current_scale(
    core: &impl WriteHost,
    cluster: &str,
    target: TargetRef,
) -> Result<i32, String> {
    let handle = core.handle(cluster)?;
    actions::current_scale(&handle, &target)
        .await
        .map_err(|e| e.to_string())
}

/// Set a workload's replica count through its scale subresource.
pub async fn scale(
    core: &impl WriteHost,
    cluster: &str,
    target: TargetRef,
    replicas: i32,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::scale(&handle, &target, replicas).await;
    core.audit(
        cluster,
        &format!("scale to {replicas}"),
        &label(&target),
        &result,
    );
    result.map(|_| ()).map_err(|e| e.to_string())
}

/// Read the replica count a person typed. Whole, non-negative, and no more
/// than the API's `int32`; anything else is said in words rather than sent.
pub fn parse_replicas(text: &str) -> Result<i32, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter a number of replicas.".into());
    }
    match text.parse::<i64>() {
        Ok(value) if value < 0 => Err("Replicas cannot be negative.".into()),
        Ok(value) => i32::try_from(value).map_err(|_| "That is too many replicas.".to_string()),
        Err(_) => Err(format!("`{text}` is not a whole number.")),
    }
}

/// Start a CronJob's Job now. Returns the new Job's name.
pub async fn trigger_cronjob(
    core: &impl WriteHost,
    cluster: &str,
    target: TargetRef,
) -> Result<String, String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::trigger_cronjob(&handle, &target).await;
    core.audit(cluster, "trigger", &label(&target), &result);
    result.map_err(|e| e.to_string())
}

/// Pause or resume a CronJob's schedule.
pub async fn set_suspended(
    core: &impl WriteHost,
    cluster: &str,
    target: TargetRef,
    suspended: bool,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::set_suspended(&handle, &target, suspended).await;
    let action = if suspended { "suspend" } else { "resume" };
    core.audit(cluster, action, &label(&target), &result);
    result.map_err(|e| e.to_string())
}

/// Mark a node (un)schedulable.
pub async fn set_cordoned(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
    cordoned: bool,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::set_cordoned(&handle, node, cordoned).await;
    let action = if cordoned { "cordon" } else { "uncordon" };
    core.audit(cluster, action, &format!("nodes/{node}"), &result);
    result.map_err(|e| e.to_string())
}

/// What draining a node would do, without cordoning it or evicting anything.
/// Reading only, so a protected cluster may be previewed.
pub async fn drain_preview(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
    delete_standalone_pods: bool,
) -> Result<DrainReport, String> {
    let handle = core.handle(cluster)?;
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
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
    delete_standalone_pods: bool,
) -> Result<DrainReport, String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
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
    core.audit(cluster, "drain", &format!("nodes/{node}"), &outcome);
    result.map_err(|e| e.to_string())
}

/// A node's labels and taints, to edit.
pub async fn read_node_tags(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
) -> Result<NodeTags, String> {
    let handle = core.handle(cluster)?;
    node_edit::read(&handle, node)
        .await
        .map_err(|e| e.to_string())
}

/// Save a node's labels and taints, refusing if the node changed since `before`
/// was read.
pub async fn save_node_tags(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
    before: &NodeTags,
    labels: &BTreeMap<String, String>,
    taints: &[Taint],
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = node_edit::apply(&handle, node, before, labels, taints).await;
    core.audit(cluster, "editNodeTags", &format!("nodes/{node}"), &result);
    result.map_err(|e| e.to_string())
}

/// What a workload is connected to, and what it and its pods have recently said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connected {
    pub related: Related,
    /// Newest last.
    pub events: Vec<EventRow>,
}

/// Pods, services, ingress addresses and events around one object. Reads only.
///
/// A workload's events include those of its pods; any other kind shows only its
/// own, so a pod's events are not listed twice.
pub async fn read_connected(
    core: &impl WriteHost,
    cluster: &str,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<Connected, String> {
    let handle = core.handle(cluster)?;
    let related = related::related(&handle, resource, namespace, name)
        .await
        .map_err(|e| e.to_string())?;
    let workload = matches!(
        resource,
        "apps/v1/deployments"
            | "apps/v1/statefulsets"
            | "apps/v1/daemonsets"
            | "apps/v1/replicasets"
            | "batch/v1/jobs"
    );
    let events = match namespace {
        Some(namespace) if workload => {
            let pods: Vec<String> = related.pods.iter().map(|pod| pod.name.clone()).collect();
            related::workload_events(&handle, namespace, name, &pods).await
        }
        namespace => related::events(&handle, namespace, name).await,
    }
    .map_err(|e| e.to_string())?;
    Ok(Connected { related, events })
}

/// How many pods a node allows and runs.
pub async fn read_pod_limit(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
) -> Result<PodLimit, String> {
    let handle = core.handle(cluster)?;
    node_pods::read(&handle, node)
        .await
        .map_err(|e| e.to_string())
}

/// Change a node's kubelet `maxPods`. Restarts the kubelet.
pub async fn set_pod_limit(
    core: &impl WriteHost,
    cluster: &str,
    node: &str,
    limit: u32,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = node_pods::set(&handle, node, limit, node_pods::POD_NAMESPACE, node).await;
    core.audit(cluster, "setMaxPods", &format!("nodes/{node}"), &result);
    result.map_err(|e| e.to_string())
}

/// Ask a GitOps controller to reconcile an object now. Writes an annotation.
pub async fn gitops_reconcile(
    core: &impl WriteHost,
    cluster: &str,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = k8s_ops::gitops::reconcile(&handle, resource, namespace, name).await;
    let target = match namespace {
        Some(namespace) => format!("{resource}/{namespace}/{name}"),
        None => format!("{resource}/{name}"),
    };
    core.audit(cluster, "gitopsReconcile", &target, &result);
    result.map_err(|e| e.to_string())
}

/// Pause or resume a Flux object's reconciliation.
pub async fn gitops_set_suspended(
    core: &impl WriteHost,
    cluster: &str,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
    suspended: bool,
) -> Result<(), String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
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
    core.audit(cluster, action, &target, &result);
    result.map_err(|e| e.to_string())
}

/// What the installed GitOps controllers manage, optionally in one namespace.
pub async fn gitops_survey(
    core: &impl WriteHost,
    cluster: &str,
    namespace: Option<&str>,
) -> Result<k8s_ops::gitops::GitOpsSummary, String> {
    let handle = core.handle(cluster)?;
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
    core: &impl WriteHost,
    cluster: &str,
    targets: Vec<TargetRef>,
) -> Result<Vec<BulkOutcome>, String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    // The count is the guard the backend asks for. The front end has already
    // shown what is about to go and asked, so it is supplied here.
    let confirmation = targets.len().to_string();
    let result = actions::delete_many(&handle, &targets, &confirmation).await;
    let audited = match &result {
        Ok(outcomes) => bulk_result(outcomes),
        Err(err) => Err(err.to_string()),
    };
    core.audit(cluster, "bulkDelete", &bulk_label(&targets), &audited);
    result.map_err(|e| e.to_string())
}

/// Roll several workloads, as `kubectl rollout restart` does.
pub async fn restart_many(
    core: &impl WriteHost,
    cluster: &str,
    targets: Vec<TargetRef>,
) -> Result<Vec<BulkOutcome>, String> {
    core.ensure_writable(cluster)?;
    let handle = core.handle(cluster)?;
    let result = actions::restart_many(&handle, &targets).await;
    let audited = match &result {
        Ok(outcomes) => bulk_result(outcomes),
        Err(err) => Err(err.to_string()),
    };
    core.audit(cluster, "bulkRestart", &bulk_label(&targets), &audited);
    result.map_err(|e| e.to_string())
}

/// Write the objects to a zip, one YAML file per object. It only reads the
/// cluster, so a protected one may be exported.
pub async fn export_archive(
    core: &impl WriteHost,
    cluster: &str,
    targets: Vec<TargetRef>,
    path: std::path::PathBuf,
) -> Result<ExportResult, String> {
    let handle = core.handle(cluster)?;
    actions::export_archive(&handle, &targets, &path)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fmt::Display,
        sync::{Arc, Mutex},
    };

    use super::{
        BulkOutcome, apply_audit, bulk_label, bulk_result, failed, parse_replicas, save_node_tags,
    };
    use crate::sources::{Audited, ClusterSource, WriteHost};
    use k8s_core::cluster::ClusterHandle;
    use k8s_ops::{actions::TargetRef, apply::ApplyOutcome, error::OpsError, node_edit::NodeTags};

    /// An app that refuses every change, as one with a protected cluster does.
    /// Reaching the cluster or the audit log would be a bug, so both are noted.
    #[derive(Default)]
    struct Protected {
        touched: Mutex<Vec<&'static str>>,
    }

    impl ClusterSource for Protected {
        fn handle(&self, _: &str) -> Result<Arc<ClusterHandle>, String> {
            if let Ok(mut touched) = self.touched.lock() {
                touched.push("cluster");
            }
            Err("the cluster was reached".into())
        }
    }

    impl Audited for Protected {
        fn audit<T, E: Display>(&self, _: &str, _: &str, _: &str, _: &Result<T, E>) {
            if let Ok(mut touched) = self.touched.lock() {
                touched.push("audit");
            }
        }
    }

    impl WriteHost for Protected {
        fn ensure_writable(&self, cluster: &str) -> Result<(), String> {
            Err(format!("`{cluster}` is protected"))
        }
    }

    #[tokio::test]
    async fn a_protected_cluster_refuses_a_change_before_reaching_it_or_auditing_it() {
        let app = Protected::default();
        let error = save_node_tags(
            &app,
            "prod",
            "node-1",
            &NodeTags::default(),
            &BTreeMap::new(),
            &[],
        )
        .await
        .expect_err("refused");
        assert_eq!(error, "`prod` is protected");
        assert!(
            app.touched.lock().expect("lock").is_empty(),
            "a refused change leaves no trace in the cluster or the log"
        );
    }

    #[test]
    fn only_an_applied_edit_is_audited_as_a_success() {
        let applied = Ok(ApplyOutcome::Applied {
            yaml: String::new(),
            resource_version: None,
        });
        assert_eq!(apply_audit(&applied), Ok(()));

        let conflict = Ok(ApplyOutcome::Conflict { conflicts: vec![] });
        assert!(apply_audit(&conflict).unwrap_err().starts_with("refused"));

        let failed: Result<ApplyOutcome, OpsError> = Err(OpsError::other("forbidden"));
        assert!(apply_audit(&failed).unwrap_err().contains("forbidden"));
    }

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
    fn replicas_are_whole_non_negative_and_fit_an_int32() {
        assert_eq!(parse_replicas(" 3 "), Ok(3));
        assert_eq!(parse_replicas("0"), Ok(0));
        for bad in ["", "  ", "-1", "2.5", "three", "2147483648"] {
            assert!(parse_replicas(bad).is_err(), "{bad:?}");
        }
        assert!(
            parse_replicas("-1").unwrap_err().contains("negative"),
            "says why"
        );
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
