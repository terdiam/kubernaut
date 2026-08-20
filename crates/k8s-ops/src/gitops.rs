//! GitOps controllers: what they are syncing, and whether it worked.
//!
//! Three controllers are recognised — Argo CD, Flux and Fleet — because each
//! stores the same idea in a different shape. The value here is not listing
//! CRDs, which the resource browser already does; it is putting "which commit,
//! from which repository, applied or not, and why not" in one line.
//!
//! Whichever are installed are shown; the rest simply do not appear.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use kube::{
    Api,
    api::{DynamicObject, ListParams, Patch, PatchParams},
    discovery::ApiResource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{OpsError, Result};

/// A kind one of the controllers uses, and how to read it.
struct Source {
    controller: &'static str,
    kind: &'static str,
    group: &'static str,
    version: &'static str,
    plural: &'static str,
    /// True when a reconcile can be requested by annotating the object.
    reconcilable: bool,
}

const SOURCES: &[Source] = &[
    Source {
        controller: "argocd",
        kind: "Application",
        group: "argoproj.io",
        version: "v1alpha1",
        plural: "applications",
        reconcilable: true,
    },
    Source {
        controller: "flux",
        kind: "Kustomization",
        group: "kustomize.toolkit.fluxcd.io",
        version: "v1",
        plural: "kustomizations",
        reconcilable: true,
    },
    Source {
        controller: "flux",
        kind: "HelmRelease",
        group: "helm.toolkit.fluxcd.io",
        version: "v2",
        plural: "helmreleases",
        reconcilable: true,
    },
    Source {
        controller: "flux",
        kind: "GitRepository",
        group: "source.toolkit.fluxcd.io",
        version: "v1",
        plural: "gitrepositories",
        reconcilable: true,
    },
    Source {
        controller: "fleet",
        kind: "GitRepo",
        group: "fleet.cattle.io",
        version: "v1alpha1",
        plural: "gitrepos",
        reconcilable: false,
    },
    Source {
        controller: "fleet",
        kind: "Bundle",
        group: "fleet.cattle.io",
        version: "v1alpha1",
        plural: "bundles",
        reconcilable: false,
    },
];

impl Source {
    fn api_resource(&self) -> ApiResource {
        ApiResource {
            group: self.group.to_string(),
            version: self.version.to_string(),
            api_version: format!("{}/{}", self.group, self.version),
            kind: self.kind.to_string(),
            plural: self.plural.to_string(),
        }
    }

    fn key(&self) -> String {
        format!("{}/{}/{}", self.group, self.version, self.plural)
    }
}

/// One thing a GitOps controller manages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOpsEntry {
    /// `argocd` | `flux` | `fleet`
    pub controller: String,
    pub kind: String,
    /// `group/version/plural`, so the UI can open the object.
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,

    /// Repository the desired state comes from.
    pub source: Option<String>,
    /// Path or chart within that source.
    pub path: Option<String>,
    /// Branch, tag or commit being tracked.
    pub target_revision: Option<String>,
    /// Revision actually applied. Differing from the target is the whole point
    /// of the screen.
    pub applied_revision: Option<String>,

    /// Short status word shown in the table.
    pub status: String,
    /// `ok` | `pending` | `warning` | `error` | `unknown`
    pub health: String,
    /// Why, when it is not healthy.
    pub message: Option<String>,
    pub last_sync: Option<String>,
    /// Reconciliation deliberately paused.
    pub suspended: bool,
    /// False when this controller has no annotation-driven reconcile.
    pub reconcilable: bool,
}

/// Which controllers this cluster actually has.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOpsSummary {
    pub controllers: Vec<String>,
    pub entries: Vec<GitOpsEntry>,
    /// Kinds that exist but could not be listed, with the reason.
    pub limitations: Vec<String>,
}

fn condition<'a>(status: Option<&'a Value>, wanted: &str) -> Option<&'a Value> {
    status?
        .get("conditions")?
        .as_array()?
        .iter()
        .find(|condition| condition.get("type").and_then(Value::as_str) == Some(wanted))
}

fn text(value: Option<&Value>, pointer: &str) -> Option<String> {
    value?
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

/// Read one object into the common shape.
fn entry_from(source: &Source, object: &DynamicObject) -> GitOpsEntry {
    let data = &object.data;
    let spec = data.get("spec");
    let status = data.get("status");

    let mut entry = GitOpsEntry {
        controller: source.controller.to_string(),
        kind: source.kind.to_string(),
        resource: source.key(),
        namespace: object.metadata.namespace.clone(),
        name: object.metadata.name.clone().unwrap_or_default(),
        source: None,
        path: None,
        target_revision: None,
        applied_revision: None,
        status: "unknown".into(),
        health: "unknown".into(),
        message: None,
        last_sync: None,
        suspended: spec
            .and_then(|spec| spec.get("suspend"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reconcilable: source.reconcilable,
    };

    match source.controller {
        "argocd" => {
            entry.source = text(spec, "/source/repoURL");
            entry.path = text(spec, "/source/path").or_else(|| text(spec, "/source/chart"));
            entry.target_revision = text(spec, "/source/targetRevision");
            entry.applied_revision = text(status, "/sync/revision");
            entry.status = text(status, "/sync/status").unwrap_or_else(|| "Unknown".into());
            entry.last_sync = text(status, "/operationState/finishedAt");

            let health = text(status, "/health/status").unwrap_or_default();
            entry.message =
                text(status, "/health/message").or_else(|| text(status, "/operationState/message"));
            entry.health = match (entry.status.as_str(), health.as_str()) {
                (_, "Degraded") | (_, "Missing") => "error",
                ("OutOfSync", _) => "warning",
                (_, "Progressing") => "pending",
                ("Synced", "Healthy") => "ok",
                _ => "unknown",
            }
            .into();
        }
        "flux" => {
            entry.source = text(spec, "/url").or_else(|| text(spec, "/sourceRef/name"));
            entry.path = text(spec, "/path").or_else(|| text(spec, "/chart/spec/chart"));
            entry.target_revision = text(spec, "/ref/branch")
                .or_else(|| text(spec, "/ref/tag"))
                .or_else(|| text(spec, "/ref/commit"));
            entry.applied_revision = text(status, "/lastAppliedRevision")
                .or_else(|| text(status, "/artifact/revision"))
                .or_else(|| text(status, "/lastAttemptedRevision"));

            // Flux states everything through the Ready condition; the reason is
            // the actionable part ("ArtifactFailed" is not the same problem as
            // "BuildFailed").
            let ready = condition(status, "Ready");
            let ready_status = ready
                .and_then(|condition| condition.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let reason = ready
                .and_then(|condition| condition.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("");

            entry.message = ready
                .and_then(|condition| condition.get("message"))
                .and_then(Value::as_str)
                .map(String::from);
            entry.last_sync = ready
                .and_then(|condition| condition.get("lastTransitionTime"))
                .and_then(Value::as_str)
                .map(String::from);

            let reconciling = condition(status, "Reconciling").is_some();
            entry.status = if entry.suspended {
                "Suspended".into()
            } else if ready_status == "True" {
                "Ready".into()
            } else if reason.is_empty() {
                "NotReady".into()
            } else {
                reason.to_string()
            };
            entry.health = match (entry.suspended, ready_status, reconciling) {
                (true, _, _) => "warning",
                (_, "True", _) => "ok",
                (_, "False", true) => "pending",
                (_, "False", false) => "error",
                _ => "unknown",
            }
            .into();
        }
        "fleet" => {
            entry.source = text(spec, "/repo");
            entry.path = spec
                .and_then(|spec| spec.get("paths"))
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|paths| !paths.is_empty());
            entry.target_revision = text(spec, "/branch").or_else(|| text(spec, "/revision"));
            entry.applied_revision = text(status, "/commit");
            entry.message = text(status, "/display/readyBundleDeployments")
                .or_else(|| text(status, "/display/state"));

            let desired = status
                .and_then(|status| status.pointer("/summary/desiredReady"))
                .and_then(Value::as_i64);
            // Fleet omits zero-valued counters, so a bundle with nothing ready
            // has no `ready` key at all. Reading that as "unknown" hid exactly
            // the state most worth seeing.
            let ready = status
                .and_then(|status| status.pointer("/summary/ready"))
                .and_then(Value::as_i64)
                .or(desired.map(|_| 0));

            let state = text(status, "/display/state");
            entry.status = match (ready, desired) {
                (Some(ready), Some(desired)) => {
                    let counts = format!("{ready}/{desired} ready");
                    match &state {
                        // The state word is the reason; the counts are the
                        // extent. Both matter.
                        Some(state) if ready != desired => format!("{state} · {counts}"),
                        _ => counts,
                    }
                }
                _ => state.clone().unwrap_or_else(|| "Unknown".into()),
            };
            entry.health = match (ready, desired) {
                (Some(ready), Some(desired)) if desired > 0 && ready == desired => "ok",
                (Some(0), Some(desired)) if desired > 0 => "error",
                (Some(_), Some(_)) => "pending",
                _ => "unknown",
            }
            .into();
        }
        _ => {}
    }

    entry
}

/// Everything the installed GitOps controllers manage.
pub async fn survey(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
) -> Result<GitOpsSummary> {
    let discovery = match cluster.discovery() {
        Some(discovery) => discovery,
        None => cluster.refresh_discovery().await?,
    };

    let mut controllers: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    let mut limitations = Vec::new();

    for source in SOURCES {
        // Only ask for kinds the cluster defines; listing a missing CRD returns
        // 404 and logs a warning for a condition that is simply "not installed".
        if discovery.get(&source.key()).is_none() {
            continue;
        }
        if !controllers.iter().any(|name| name == source.controller) {
            controllers.push(source.controller.to_string());
        }

        let resource = source.api_resource();
        let api: Api<DynamicObject> = match namespace {
            Some(ns) => Api::namespaced_with(cluster.client.clone(), ns, &resource),
            None => Api::all_with(cluster.client.clone(), &resource),
        };

        match api.list(&ListParams::default()).await {
            Ok(list) => entries.extend(list.items.iter().map(|object| entry_from(source, object))),
            Err(err) => limitations.push(format!("could not list {}: {err}", source.kind)),
        }
    }

    // Unhealthy first: the reason to open this screen is that something is not
    // applying.
    entries.sort_by(|a, b| {
        let rank = |health: &str| match health {
            "error" => 0,
            "warning" => 1,
            "pending" => 2,
            "ok" => 3,
            _ => 4,
        };
        rank(&a.health)
            .cmp(&rank(&b.health))
            .then_with(|| a.controller.cmp(&b.controller))
            .then_with(|| a.namespace.cmp(&b.namespace))
            .then_with(|| a.name.cmp(&b.name))
    });

    controllers.sort();
    Ok(GitOpsSummary {
        controllers,
        entries,
        limitations,
    })
}

/// Ask a controller to reconcile now.
///
/// Both Argo CD and Flux take this as an annotation, which is why it works
/// without their CLIs — but it is still a write to the object, so the caller
/// confirms first.
pub async fn reconcile(
    cluster: &Arc<ClusterHandle>,
    entry_resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<()> {
    let source = SOURCES
        .iter()
        .find(|source| source.key() == entry_resource)
        .ok_or_else(|| OpsError::other(format!("`{entry_resource}` is not a GitOps kind")))?;

    if !source.reconcilable {
        return Err(OpsError::other(format!(
            "{} has no annotation-driven reconcile; trigger it from its controller",
            source.kind
        )));
    }

    let now = k8s_openapi::jiff::Timestamp::now().to_string();
    let patch = match source.controller {
        "argocd" => json!({
            "metadata": { "annotations": { "argocd.argoproj.io/refresh": "normal" } }
        }),
        // Flux watches this annotation's *value*, so it must change each time.
        _ => json!({
            "metadata": { "annotations": { "reconcile.fluxcd.io/requestedAt": now } }
        }),
    };

    let resource = source.api_resource();
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(cluster.client.clone(), ns, &resource),
        None => Api::all_with(cluster.client.clone(), &resource),
    };

    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

/// Pause or resume reconciliation. Flux only; Argo CD expresses this through
/// its sync policy rather than a field.
pub async fn set_suspended(
    cluster: &Arc<ClusterHandle>,
    entry_resource: &str,
    namespace: Option<&str>,
    name: &str,
    suspended: bool,
) -> Result<()> {
    let source = SOURCES
        .iter()
        .find(|source| source.key() == entry_resource)
        .ok_or_else(|| OpsError::other(format!("`{entry_resource}` is not a GitOps kind")))?;

    if source.controller != "flux" {
        return Err(OpsError::other(format!(
            "{} does not support suspending from here",
            source.kind
        )));
    }

    let resource = source.api_resource();
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(cluster.client.clone(), ns, &resource),
        None => Api::all_with(cluster.client.clone(), &resource),
    };

    api.patch(
        name,
        &PatchParams::default(),
        &Patch::Merge(&json!({ "spec": { "suspend": suspended } })),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::{DynamicObject, ObjectMeta};

    fn object(data: Value) -> DynamicObject {
        DynamicObject {
            types: None,
            metadata: ObjectMeta {
                name: Some("thing".into()),
                namespace: Some("flux-system".into()),
                ..Default::default()
            },
            data,
        }
    }

    fn flux_kustomization() -> &'static Source {
        SOURCES
            .iter()
            .find(|source| source.kind == "Kustomization")
            .unwrap()
    }

    /// The failing Kustomization in the development cluster: reconciling *and*
    /// not ready. That is retrying, not broken beyond hope, so it reads as
    /// pending rather than error.
    #[test]
    fn flux_retrying_reads_as_pending_with_its_reason() {
        let entry = entry_from(
            flux_kustomization(),
            &object(json!({
                "spec": {"path": "./kratix-requests", "sourceRef": {"name": "kratix-requests"}},
                "status": {"conditions": [
                    {"type": "Reconciling", "status": "True", "reason": "ProgressingWithRetry"},
                    {"type": "Ready", "status": "False", "reason": "ArtifactFailed",
                     "message": "kustomization path not found"}
                ]}
            })),
        );

        assert_eq!(entry.health, "pending");
        assert_eq!(
            entry.status, "ArtifactFailed",
            "the reason is the useful part"
        );
        assert!(entry.message.unwrap().contains("path not found"));
        assert_eq!(entry.path.as_deref(), Some("./kratix-requests"));
    }

    #[test]
    fn flux_ready_reads_as_healthy_with_its_revision() {
        let entry = entry_from(
            flux_kustomization(),
            &object(json!({
                "spec": {"path": "./resources"},
                "status": {
                    "lastAppliedRevision": "main@sha1:a806440",
                    "conditions": [{"type": "Ready", "status": "True", "reason": "ReconciliationSucceeded"}]
                }
            })),
        );
        assert_eq!(entry.health, "ok");
        assert_eq!(entry.status, "Ready");
        assert_eq!(entry.applied_revision.as_deref(), Some("main@sha1:a806440"));
    }

    /// Suspended is neither healthy nor broken: nothing is being applied, and
    /// showing it green would hide that.
    #[test]
    fn suspended_flux_objects_are_flagged() {
        let entry = entry_from(
            flux_kustomization(),
            &object(json!({
                "spec": {"suspend": true},
                "status": {"conditions": [{"type": "Ready", "status": "True"}]}
            })),
        );
        assert!(entry.suspended);
        assert_eq!(entry.status, "Suspended");
        assert_eq!(entry.health, "warning");
    }

    #[test]
    fn argo_out_of_sync_is_a_warning_and_degraded_is_an_error() {
        let argo = SOURCES.iter().find(|s| s.controller == "argocd").unwrap();

        let drifted = entry_from(
            argo,
            &object(json!({
                "spec": {"source": {"repoURL": "https://git.example/app", "path": "deploy",
                                    "targetRevision": "main"}},
                "status": {"sync": {"status": "OutOfSync", "revision": "abc123"},
                           "health": {"status": "Healthy"}}
            })),
        );
        assert_eq!(drifted.health, "warning");
        assert_eq!(drifted.source.as_deref(), Some("https://git.example/app"));

        let broken = entry_from(
            argo,
            &object(json!({"status": {"sync": {"status": "Synced"},
                                  "health": {"status": "Degraded", "message": "pod crashloop"}}})),
        );
        assert_eq!(broken.health, "error");
        assert_eq!(broken.message.as_deref(), Some("pod crashloop"));
    }

    #[test]
    fn fleet_summarises_ready_against_desired() {
        let fleet = SOURCES.iter().find(|s| s.kind == "Bundle").unwrap();

        let partial = entry_from(
            fleet,
            &object(json!({"status": {"summary": {"ready": 1, "desiredReady": 3}}})),
        );
        assert_eq!(partial.status, "1/3 ready");
        assert_eq!(partial.health, "pending");

        let done = entry_from(
            fleet,
            &object(json!({"status": {"summary": {"ready": 3, "desiredReady": 3}}})),
        );
        assert_eq!(done.health, "ok");
    }

    /// Fleet leaves out counters that are zero. Treating a missing `ready` as
    /// "unknown" hid bundles where nothing had applied at all — the exact
    /// state the screen exists to surface.
    #[test]
    fn fleet_omits_zero_counters_and_that_is_still_an_error() {
        let fleet = SOURCES.iter().find(|s| s.kind == "Bundle").unwrap();
        let entry = entry_from(
            fleet,
            &object(json!({"status": {
                "summary": {"desiredReady": 1, "waitApplied": 1},
                "display": {"state": "WaitApplied", "readyClusters": "0/1"}
            }})),
        );
        assert_eq!(entry.health, "error");
        assert_eq!(entry.status, "WaitApplied · 0/1 ready");
    }

    #[test]
    fn fleet_cannot_be_reconciled_by_annotation() {
        let fleet = SOURCES.iter().find(|s| s.kind == "Bundle").unwrap();
        assert!(!fleet.reconcilable);
    }
}
