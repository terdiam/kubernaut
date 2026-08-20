//! Editing objects: server-side apply with a real diff preview.
//!
//! Every write goes through server-side apply with a stable field manager, so
//! the app owns exactly the fields it sets and conflicts with other managers
//! (a controller, another operator, `kubectl`) are reported instead of being
//! silently overwritten. The preview is produced by a `dryRun=All` apply, which
//! means the diff shown is what the apiserver would actually do — defaulting,
//! admission webhooks and all — rather than a guess made client-side.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use kube::{
    Api,
    api::{DynamicObject, Patch, PatchParams},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OpsError, Result};

/// Field manager recorded on every field this app owns.
pub const FIELD_MANAGER: &str = "kubernaut";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRequest {
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,
    /// Full object as YAML (what the editor holds).
    pub yaml: String,
    /// Take ownership of fields another manager owns. Off by default: a forced
    /// apply can revert a controller's work.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffResult {
    /// Unified diff, live → proposed. Empty when nothing changes.
    pub unified: String,
    pub changed: bool,
    /// Fields another manager owns; apply refuses them unless forced.
    pub conflicts: Vec<FieldConflict>,
}

/// One field an apply would take from another field manager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldConflict {
    /// Field manager that owns the field, e.g. `rancher`.
    pub manager: String,
    /// Field path as the apiserver reports it, e.g.
    /// `.spec.template.spec.containers[name="backend"].image`. Empty when the
    /// apiserver only named the manager.
    pub field: String,
}

/// What an apply did.
///
/// A field-manager conflict is an answer, not a failure: the user has to decide
/// whether taking the field from its owner is right. Reporting it as an error
/// left the form with a wall of Rust debug output and no way forward.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ApplyOutcome {
    Applied {
        yaml: String,
        #[serde(rename = "resourceVersion")]
        resource_version: Option<String>,
    },
    Conflict {
        conflicts: Vec<FieldConflict>,
    },
}

/// Parse editor YAML into an object, checking it matches the target.
fn parse(request: &EditRequest) -> Result<DynamicObject> {
    let mut value: Value =
        serde_yaml_ng::from_str(&request.yaml).map_err(|err| OpsError::Yaml(err.to_string()))?;

    let name = value
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != request.name {
        return Err(OpsError::other(format!(
            "metadata.name is `{name}` but this editor is bound to `{}`. \
             Renaming creates a new object — create it explicitly instead.",
            request.name
        )));
    }
    if value.pointer("/apiVersion").is_none() || value.pointer("/kind").is_none() {
        return Err(OpsError::Yaml(
            "apiVersion and kind are required for an apply".into(),
        ));
    }

    // The apiserver rejects an apply carrying `metadata.managedFields`
    // ("metadata.managedFields must be nil"): ownership is its bookkeeping, not
    // ours to send back. The form editor round-trips the whole live object, so
    // it arrives here unless we drop it.
    if let Some(meta) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        meta.remove("managedFields");
    }

    serde_json::from_value(value).map_err(|err| OpsError::Yaml(err.to_string()))
}

async fn api_for(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: Option<&str>,
) -> Result<Api<DynamicObject>> {
    let discovery = match cluster.discovery() {
        Some(d) => d,
        None => cluster.refresh_discovery().await?,
    };
    let descriptor = discovery.require(resource)?;
    let ar = descriptor.api_resource();
    Ok(match (namespace, descriptor.namespaced) {
        (Some(ns), true) => Api::namespaced_with(cluster.client.clone(), ns, &ar),
        _ => Api::all_with(cluster.client.clone(), &ar),
    })
}

/// What the apiserver would change, without changing it.
pub async fn preview(cluster: &Arc<ClusterHandle>, request: &EditRequest) -> Result<DiffResult> {
    let object = parse(request)?;
    let api = api_for(cluster, &request.resource, request.namespace.as_deref()).await?;

    let live = api.get(&request.name).await?;
    let live_yaml = render(&live)?;

    let mut params = PatchParams::apply(FIELD_MANAGER).dry_run();
    if request.force {
        params = params.force();
    }

    match api
        .patch(&request.name, &params, &Patch::Apply(&object))
        .await
    {
        Ok(proposed) => {
            let proposed_yaml = render(&proposed)?;
            let unified = unified_diff(&live_yaml, &proposed_yaml, &request.name);
            Ok(DiffResult {
                changed: !unified.is_empty(),
                unified,
                conflicts: Vec::new(),
            })
        }
        Err(kube::Error::Api(status)) if status.code == 409 => {
            // Field-manager conflict: report who owns the contested fields so
            // the user can decide whether forcing is appropriate.
            Ok(DiffResult {
                unified: String::new(),
                changed: true,
                conflicts: conflicts_from(&status),
            })
        }
        Err(err) => Err(err.into()),
    }
}

/// Apply for real.
pub async fn apply(cluster: &Arc<ClusterHandle>, request: &EditRequest) -> Result<ApplyOutcome> {
    let object = parse(request)?;
    let api = api_for(cluster, &request.resource, request.namespace.as_deref()).await?;

    let mut params = PatchParams::apply(FIELD_MANAGER);
    if request.force {
        params = params.force();
    }

    match api
        .patch(&request.name, &params, &Patch::Apply(&object))
        .await
    {
        Ok(applied) => Ok(ApplyOutcome::Applied {
            yaml: render(&applied)?,
            resource_version: applied.metadata.resource_version.clone(),
        }),
        Err(kube::Error::Api(status)) if status.code == 409 => Ok(ApplyOutcome::Conflict {
            conflicts: conflicts_from(&status),
        }),
        Err(err) => Err(err.into()),
    }
}

/// Replace the object wholesale, for the "recreate" path where apply cannot
/// express the change (immutable fields).
pub async fn replace(cluster: &Arc<ClusterHandle>, request: &EditRequest) -> Result<ApplyOutcome> {
    let object = parse(request)?;
    let api = api_for(cluster, &request.resource, request.namespace.as_deref()).await?;
    let replaced = api
        .replace(&request.name, &Default::default(), &object)
        .await?;
    Ok(ApplyOutcome::Applied {
        yaml: render(&replaced)?,
        resource_version: replaced.metadata.resource_version.clone(),
    })
}

pub(crate) fn render(obj: &DynamicObject) -> Result<String> {
    Ok(k8s_core::objects::to_yaml(obj, false)?)
}

/// Line-level unified diff with three lines of context.
pub(crate) fn unified_diff(before: &str, after: &str, name: &str) -> String {
    if before == after {
        return String::new();
    }
    let diff = similar::TextDiff::from_lines(before, after);
    diff.unified_diff()
        .context_radius(3)
        .header(&format!("{name} (live)"), &format!("{name} (proposed)"))
        .to_string()
}

/// Which fields the apiserver refused, and to whom they belong.
///
/// The status carries one cause per contested field
/// (`reason: FieldManagerConflict`), which is exact. Older or proxied
/// apiservers send only the prose message, so that is parsed as a fallback:
/// `Apply failed with 1 conflict: conflict with "rancher" using apps/v1: .spec.replicas`
pub(crate) fn conflicts_from(status: &kube::core::Status) -> Vec<FieldConflict> {
    let mut out: Vec<FieldConflict> = status
        .details
        .iter()
        .flat_map(|details| details.causes.iter())
        .filter_map(|cause| {
            let manager = manager_name(&cause.message)?;
            Some(FieldConflict {
                manager,
                field: cause.field.clone(),
            })
        })
        .collect();

    if out.is_empty() {
        out = parse_conflicts(&status.message);
    }
    out.sort_by(|a, b| a.manager.cmp(&b.manager).then(a.field.cmp(&b.field)));
    out.dedup();
    out
}

/// The manager in `conflict with "rancher" using apps/v1`.
fn manager_name(message: &str) -> Option<String> {
    let start = message.find("conflict with \"")? + "conflict with \"".len();
    let rest = &message[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The field path at the start of `token`, up to the comma that separates it
/// from the next clause. Commas inside `[name="x",port=80]` do not separate.
fn field_path(token: &str) -> String {
    let mut depth = 0usize;
    for (index, ch) in token.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return token[..index].to_string(),
            _ => {}
        }
    }
    token.to_string()
}

/// Fallback parse of the prose conflict message.
///
/// One manager per `with "..."`, then every field path mentioned before the
/// next manager. The wording varies with the number of conflicts (`conflict
/// with` / `conflicts with`, inline field or bullet list), so only the quoted
/// manager and the leading `.` of a field path are relied on.
fn parse_conflicts(message: &str) -> Vec<FieldConflict> {
    let mut out: Vec<FieldConflict> = Vec::new();
    let mut rest = message;
    while let Some(start) = rest.find("with \"") {
        rest = &rest[start + "with \"".len()..];
        let Some(end) = rest.find('"') else { break };
        let manager = rest[..end].to_string();
        rest = &rest[end + 1..];

        let chunk = &rest[..rest.find("with \"").unwrap_or(rest.len())];
        let fields: Vec<String> = chunk
            .lines()
            .map(|line| line.rsplit_once(": ").map_or(line, |(_, tail)| tail))
            .map(|token| field_path(token.trim().trim_start_matches("- ").trim()))
            .filter(|token| token.starts_with('.'))
            .collect();

        if fields.is_empty() {
            out.push(FieldConflict {
                manager,
                field: String::new(),
            });
        } else {
            for field in fields {
                out.push(FieldConflict {
                    manager: manager.clone(),
                    field,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(yaml: &str) -> EditRequest {
        EditRequest {
            resource: "apps/v1/deployments".into(),
            namespace: Some("production".into()),
            name: "web".into(),
            yaml: yaml.into(),
            force: false,
        }
    }

    /// The form editor sends the live object back verbatim, managedFields and
    /// all; the apiserver answers `metadata.managedFields must be nil`.
    #[test]
    fn parse_drops_managed_fields() {
        let parsed = parse(&request(
            r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: production
  managedFields:
    - manager: kube-controller-manager
      operation: Update
      apiVersion: apps/v1
spec:
  replicas: 3
"#,
        ))
        .expect("parses");

        assert!(parsed.metadata.managed_fields.is_none());
        assert_eq!(parsed.metadata.name.as_deref(), Some("web"));
        assert_eq!(parsed.data.pointer("/spec/replicas"), Some(&Value::from(3)));
    }

    #[test]
    fn parse_rejects_a_rename() {
        let err = parse(&request(
            "apiVersion: apps/v1
kind: Deployment
metadata:
  name: other
",
        ))
        .unwrap_err();
        assert!(err.to_string().contains("bound to `web`"), "{err}");
    }

    #[test]
    fn diff_is_empty_for_identical_documents() {
        assert!(unified_diff("a: 1\n", "a: 1\n", "x").is_empty());
    }

    #[test]
    fn diff_shows_changed_lines() {
        let diff = unified_diff("replicas: 1\n", "replicas: 3\n", "web");
        assert!(diff.contains("-replicas: 1"), "{diff}");
        assert!(diff.contains("+replicas: 3"), "{diff}");
    }

    fn conflict(manager: &str, field: &str) -> FieldConflict {
        FieldConflict {
            manager: manager.into(),
            field: field.into(),
        }
    }

    /// Verbatim from the apiserver, editing a Rancher-managed Deployment.
    #[test]
    fn conflicts_come_from_the_status_causes() {
        let status: kube::core::Status = serde_json::from_value(serde_json::json!({
            "status": "Failure",
            "code": 409,
            "reason": "Conflict",
            "message": "Apply failed with 1 conflict: conflict with \"rancher\" using \
                        apps/v1: .spec.template.spec.containers[name=\"backend\"].image",
            "details": {
                "causes": [{
                    "reason": "FieldManagerConflict",
                    "message": "conflict with \"rancher\" using apps/v1",
                    "field": ".spec.template.spec.containers[name=\"backend\"].image"
                }]
            }
        }))
        .expect("status parses");

        assert_eq!(
            conflicts_from(&status),
            vec![conflict(
                "rancher",
                ".spec.template.spec.containers[name=\"backend\"].image"
            )]
        );
    }

    /// Without causes, the prose message is all there is.
    #[test]
    fn conflicts_fall_back_to_the_message() {
        let status: kube::core::Status = serde_json::from_value(serde_json::json!({
            "status": "Failure",
            "code": 409,
            "reason": "Conflict",
            "message": "Apply failed with 2 conflicts: conflict with \
                        \"kubectl-client-side-apply\": .spec.replicas, conflict with \
                        \"deployment-controller\": .spec.template",
        }))
        .expect("status parses");

        assert_eq!(
            conflicts_from(&status),
            vec![
                conflict("deployment-controller", ".spec.template"),
                conflict("kubectl-client-side-apply", ".spec.replicas"),
            ]
        );
    }

    #[test]
    fn a_manager_with_several_fields_is_listed_once_per_field() {
        let parsed = parse_conflicts(
            "Apply failed with 2 conflicts: conflicts with \"rancher\" using apps/v1:\n\
             - .spec.replicas\n\
             - .spec.template.spec.containers[name=\"backend\"].image",
        );
        assert_eq!(
            parsed,
            vec![
                conflict("rancher", ".spec.replicas"),
                conflict(
                    "rancher",
                    ".spec.template.spec.containers[name=\"backend\"].image"
                ),
            ]
        );
    }

    #[test]
    fn an_unparseable_message_yields_no_fields() {
        assert!(parse_conflicts("something odd").is_empty());
    }

    #[test]
    fn rename_is_rejected() {
        let request = EditRequest {
            resource: "apps/v1/deployments".into(),
            namespace: Some("default".into()),
            name: "web".into(),
            yaml: "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web-2\n".into(),
            force: false,
        };
        let err = parse(&request).unwrap_err().to_string();
        assert!(err.contains("Renaming creates a new object"), "{err}");
    }

    #[test]
    fn missing_kind_is_rejected() {
        let request = EditRequest {
            resource: "apps/v1/deployments".into(),
            namespace: Some("default".into()),
            name: "web".into(),
            yaml: "metadata:\n  name: web\n".into(),
            force: false,
        };
        assert!(parse(&request).is_err());
    }
}
