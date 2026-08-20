//! What else is connected to this object.
//!
//! The detail pane answers "what is this" well enough from the manifest. The
//! questions that actually come up during an incident are "which pods does it
//! have", "what routes to it" and "what did it just say" — all of which live in
//! other objects.

use std::{collections::BTreeMap, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::{
    autoscaling::v2::HorizontalPodAutoscaler,
    core::v1::{Event, Pod, Service},
    networking::v1::Ingress,
    policy::v1::PodDisruptionBudget,
};
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// An event as the UI lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    /// `Normal` or `Warning`.
    pub kind: String,
    pub reason: String,
    pub message: String,
    pub count: i32,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub source: Option<String>,
    /// Object the event is about, for events gathered across a workload.
    pub object: String,
}

/// A pointer to another object, enough to render a row and navigate to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedRef {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    /// `group/version/plural`, so a click can open it.
    pub resource: String,
    /// One line of context (phase, type, capacity…).
    pub detail: Option<String>,
    /// `ok` | `pending` | `warning` | `error` | `unknown`
    pub health: String,
}

/// Everything connected to one object, grouped the way people look for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Related {
    /// Pods the object owns or is.
    pub pods: Vec<RelatedRef>,
    /// Services selecting those pods.
    pub services: Vec<RelatedRef>,
    /// Ingresses routing to those services.
    pub ingresses: Vec<RelatedRef>,
    /// Owners and controllers above this object.
    pub controllers: Vec<RelatedRef>,
    /// ConfigMaps and Secrets the pod template consumes.
    pub config: Vec<RelatedRef>,
    /// PersistentVolumeClaims mounted.
    pub storage: Vec<RelatedRef>,
    /// HorizontalPodAutoscalers and PodDisruptionBudgets that apply.
    pub policies: Vec<RelatedRef>,
    /// Nodes the pods run on.
    pub nodes: Vec<RelatedRef>,
}

/// Events about an object, newest last.
///
/// Filtering happens server-side with a field selector so a busy cluster does
/// not ship every event in the namespace across the wire.
pub async fn events(
    cluster: &Arc<ClusterHandle>,
    namespace: Option<&str>,
    name: &str,
) -> Result<Vec<EventRow>> {
    let api: Api<Event> = match namespace {
        Some(ns) => Api::namespaced(cluster.client.clone(), ns),
        None => Api::all(cluster.client.clone()),
    };

    let params = ListParams::default()
        .fields(&format!("involvedObject.name={name}"))
        .limit(200);
    let list = api.list(&params).await?;

    let mut rows: Vec<EventRow> = list.iter().map(row_from_event).collect();
    rows.sort_by(|a, b| a.last_seen.cmp(&b.last_seen));
    Ok(rows)
}

/// Events for a set of pods, so a workload shows what its replicas are saying.
pub async fn events_for_pods(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    pods: &[String],
) -> Result<Vec<EventRow>> {
    let api: Api<Event> = Api::namespaced(cluster.client.clone(), namespace);
    let mut rows = Vec::new();

    // One request per pod: `involvedObject.name` takes a single value, and
    // listing the whole namespace would be far more data on a busy cluster.
    for pod in pods.iter().take(20) {
        let params = ListParams::default()
            .fields(&format!("involvedObject.name={pod}"))
            .limit(50);
        if let Ok(list) = api.list(&params).await {
            rows.extend(list.iter().map(row_from_event));
        }
    }

    rows.sort_by(|a, b| a.last_seen.cmp(&b.last_seen));
    Ok(rows)
}

fn row_from_event(event: &Event) -> EventRow {
    EventRow {
        kind: event.type_.clone().unwrap_or_else(|| "Normal".into()),
        reason: event.reason.clone().unwrap_or_default(),
        message: event.message.clone().unwrap_or_default(),
        count: event.count.unwrap_or(1),
        first_seen: event.first_timestamp.as_ref().map(|t| t.0.to_string()),
        last_seen: event
            .last_timestamp
            .as_ref()
            .or(event.event_time.as_ref().map(|t| unsafe {
                // `MicroTime` and `Time` wrap the same instant; only the
                // wrapper differs.
                &*(t as *const _ as *const k8s_openapi::apimachinery::pkg::apis::meta::v1::Time)
            }))
            .map(|t| t.0.to_string()),
        source: event
            .source
            .as_ref()
            .and_then(|s| s.component.clone().or(s.host.clone())),
        object: event
            .involved_object
            .name
            .clone()
            .unwrap_or_else(|| event.name_any()),
    }
}

fn pod_health(pod: &Pod) -> (String, Option<String>) {
    let status = pod.status.as_ref();
    let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("Unknown");

    for container in status
        .iter()
        .flat_map(|s| s.container_statuses.iter().flatten())
    {
        if let Some(reason) = container
            .state
            .as_ref()
            .and_then(|s| s.waiting.as_ref())
            .and_then(|w| w.reason.as_deref())
        {
            return (
                "error".into(),
                Some(format!("{}: {reason}", container.name)),
            );
        }
    }

    let ready = status
        .and_then(|s| s.container_statuses.as_ref())
        .map(|list| (list.iter().filter(|c| c.ready).count(), list.len()))
        .unwrap_or((0, 0));
    let detail = Some(format!("{phase} · {}/{} ready", ready.0, ready.1));

    let health = match phase {
        "Running" if ready.1 > 0 && ready.0 == ready.1 => "ok",
        "Succeeded" => "ok",
        "Running" | "Pending" => "pending",
        "Failed" => "error",
        _ => "unknown",
    };
    (health.into(), detail)
}

/// Labels a selector must match, taken from the object itself.
fn selector_of(object: &Value) -> BTreeMap<String, String> {
    object
        .pointer("/spec/selector/matchLabels")
        .or_else(|| object.pointer("/spec/selector"))
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn matches(selector: &BTreeMap<String, String>, labels: &BTreeMap<String, String>) -> bool {
    !selector.is_empty()
        && selector
            .iter()
            .all(|(key, value)| labels.get(key) == Some(value))
}

/// ConfigMaps, Secrets and PVCs a pod spec consumes.
fn referenced(spec: Option<&Value>) -> (Vec<(String, String)>, Vec<String>) {
    let mut config: Vec<(String, String)> = Vec::new();
    let mut claims: Vec<String> = Vec::new();
    let Some(spec) = spec else {
        return (config, claims);
    };

    let containers = spec
        .get("containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            spec.get("initContainers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );

    for container in containers {
        for source in container
            .get("envFrom")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = source.pointer("/configMapRef/name").and_then(Value::as_str) {
                config.push(("ConfigMap".into(), name.to_string()));
            }
            if let Some(name) = source.pointer("/secretRef/name").and_then(Value::as_str) {
                config.push(("Secret".into(), name.to_string()));
            }
        }
        for variable in container
            .get("env")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = variable
                .pointer("/valueFrom/configMapKeyRef/name")
                .and_then(Value::as_str)
            {
                config.push(("ConfigMap".into(), name.to_string()));
            }
            if let Some(name) = variable
                .pointer("/valueFrom/secretKeyRef/name")
                .and_then(Value::as_str)
            {
                config.push(("Secret".into(), name.to_string()));
            }
        }
    }

    for volume in spec
        .get("volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = volume.pointer("/configMap/name").and_then(Value::as_str) {
            config.push(("ConfigMap".into(), name.to_string()));
        }
        if let Some(name) = volume.pointer("/secret/secretName").and_then(Value::as_str) {
            config.push(("Secret".into(), name.to_string()));
        }
        if let Some(name) = volume
            .pointer("/persistentVolumeClaim/claimName")
            .and_then(Value::as_str)
        {
            claims.push(name.to_string());
        }
        for projected in volume
            .pointer("/projected/sources")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = projected.pointer("/configMap/name").and_then(Value::as_str) {
                config.push(("ConfigMap".into(), name.to_string()));
            }
            if let Some(name) = projected.pointer("/secret/name").and_then(Value::as_str) {
                config.push(("Secret".into(), name.to_string()));
            }
        }
    }

    config.sort();
    config.dedup();
    claims.sort();
    claims.dedup();
    (config, claims)
}

/// Build the related-resources view for one object.
pub async fn related(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<Related> {
    let object = k8s_core::objects::get(cluster, resource, namespace, name).await?;
    let value = k8s_core::objects::to_value(&object)?;
    let kind = object
        .types
        .as_ref()
        .map(|t| t.kind.clone())
        .unwrap_or_default();

    let mut out = Related::default();
    let Some(namespace) = namespace else {
        return Ok(out);
    };

    // Owners, walked one level — enough to get from Pod to ReplicaSet to
    // Deployment without turning the panel into a tree.
    for owner in object.metadata.owner_references.iter().flatten() {
        out.controllers.push(RelatedRef {
            resource: resource_for_kind(&owner.kind).unwrap_or("").to_string(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
            namespace: Some(namespace.to_string()),
            detail: Some("owner".into()),
            health: "unknown".into(),
        });
    }

    // The pod set: either this pod, or everything the selector matches.
    let pods_api: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    let pods: Vec<Pod> = if kind == "Pod" {
        vec![serde_json::from_value(value.clone())?]
    } else {
        let selector = selector_of(&value);
        if selector.is_empty() {
            Vec::new()
        } else {
            let label = selector
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            pods_api
                .list(&ListParams::default().labels(&label))
                .await
                .map(|list| list.items)
                .unwrap_or_default()
        }
    };

    for pod in &pods {
        let (health, detail) = pod_health(pod);
        out.pods.push(RelatedRef {
            kind: "Pod".into(),
            name: pod.name_any(),
            namespace: Some(namespace.to_string()),
            resource: "core/v1/pods".into(),
            detail,
            health,
        });
        if let Some(node) = pod.spec.as_ref().and_then(|s| s.node_name.clone())
            && !out.nodes.iter().any(|n| n.name == node)
        {
            out.nodes.push(RelatedRef {
                kind: "Node".into(),
                name: node,
                namespace: None,
                resource: "core/v1/nodes".into(),
                detail: None,
                health: "unknown".into(),
            });
        }
    }

    // Services selecting any of those pods.
    let pod_labels: Vec<BTreeMap<String, String>> = pods
        .iter()
        .map(|pod| {
            pod.labels()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .collect();

    let services: Api<Service> = Api::namespaced(cluster.client.clone(), namespace);
    let mut service_names: Vec<String> = Vec::new();
    if let Ok(list) = services.list(&ListParams::default()).await {
        for service in list {
            let selector: BTreeMap<String, String> = service
                .spec
                .as_ref()
                .and_then(|s| s.selector.clone())
                .map(|s| s.into_iter().collect())
                .unwrap_or_default();
            if !pod_labels.iter().any(|labels| matches(&selector, labels)) {
                continue;
            }
            let name = service.name_any();
            service_names.push(name.clone());
            out.services.push(RelatedRef {
                kind: "Service".into(),
                name,
                namespace: Some(namespace.to_string()),
                resource: "core/v1/services".into(),
                detail: service.spec.as_ref().and_then(|s| s.type_.clone()),
                health: "ok".into(),
            });
        }
    }

    // Ingresses routing to those services.
    let ingresses: Api<Ingress> = Api::namespaced(cluster.client.clone(), namespace);
    if let Ok(list) = ingresses.list(&ListParams::default()).await {
        for ingress in list {
            let mut backends: Vec<String> = Vec::new();
            if let Some(spec) = &ingress.spec {
                if let Some(default) = spec
                    .default_backend
                    .as_ref()
                    .and_then(|b| b.service.as_ref())
                {
                    backends.push(default.name.clone());
                }
                for rule in spec.rules.iter().flatten() {
                    for path in rule.http.iter().flat_map(|h| h.paths.iter()) {
                        if let Some(service) = path.backend.service.as_ref() {
                            backends.push(service.name.clone());
                        }
                    }
                }
            }
            if !backends.iter().any(|b| service_names.contains(b)) {
                continue;
            }
            let hosts: Vec<String> = ingress
                .spec
                .as_ref()
                .and_then(|s| s.rules.as_ref())
                .map(|rules| rules.iter().filter_map(|r| r.host.clone()).collect())
                .unwrap_or_default();
            out.ingresses.push(RelatedRef {
                kind: "Ingress".into(),
                name: ingress.name_any(),
                namespace: Some(namespace.to_string()),
                resource: "networking.k8s.io/v1/ingresses".into(),
                detail: Some(if hosts.is_empty() {
                    "*".into()
                } else {
                    hosts.join(", ")
                }),
                health: "ok".into(),
            });
        }
    }

    // Config and storage referenced by the pod template (or the pod itself).
    let spec = value
        .pointer("/spec/template/spec")
        .or_else(|| value.pointer("/spec"));
    let (config, claims) = referenced(spec);
    for (kind, name) in config {
        let resource = if kind == "Secret" {
            "core/v1/secrets"
        } else {
            "core/v1/configmaps"
        };
        out.config.push(RelatedRef {
            kind,
            name,
            namespace: Some(namespace.to_string()),
            resource: resource.into(),
            detail: None,
            health: "unknown".into(),
        });
    }
    for claim in claims {
        out.storage.push(RelatedRef {
            kind: "PersistentVolumeClaim".into(),
            name: claim,
            namespace: Some(namespace.to_string()),
            resource: "core/v1/persistentvolumeclaims".into(),
            detail: None,
            health: "unknown".into(),
        });
    }

    // Autoscalers and disruption budgets that apply to this workload.
    if kind != "Pod" {
        let hpas: Api<HorizontalPodAutoscaler> = Api::namespaced(cluster.client.clone(), namespace);
        if let Ok(list) = hpas.list(&ListParams::default()).await {
            for hpa in list {
                let target = &hpa.spec.scale_target_ref;
                if target.kind == kind && target.name == name {
                    out.policies.push(RelatedRef {
                        kind: "HorizontalPodAutoscaler".into(),
                        name: hpa.name_any(),
                        namespace: Some(namespace.to_string()),
                        resource: "autoscaling/v2/horizontalpodautoscalers".into(),
                        detail: Some(format!(
                            "{}–{} replicas",
                            hpa.spec.min_replicas.unwrap_or(1),
                            hpa.spec.max_replicas
                        )),
                        health: "ok".into(),
                    });
                }
            }
        }

        let pdbs: Api<PodDisruptionBudget> = Api::namespaced(cluster.client.clone(), namespace);
        if let Ok(list) = pdbs.list(&ListParams::default()).await {
            for pdb in list {
                let selector: BTreeMap<String, String> = pdb
                    .spec
                    .as_ref()
                    .and_then(|s| s.selector.as_ref())
                    .and_then(|s| s.match_labels.clone())
                    .map(|s| s.into_iter().collect())
                    .unwrap_or_default();
                if pod_labels.iter().any(|labels| matches(&selector, labels)) {
                    out.policies.push(RelatedRef {
                        kind: "PodDisruptionBudget".into(),
                        name: pdb.name_any(),
                        namespace: Some(namespace.to_string()),
                        resource: "policy/v1/poddisruptionbudgets".into(),
                        detail: pdb.status.as_ref().map(|s| {
                            format!(
                                "{} healthy, {} desired",
                                s.current_healthy.unwrap_or_default(),
                                s.desired_healthy.unwrap_or_default()
                            )
                        }),
                        health: "ok".into(),
                    });
                }
            }
        }
    }

    Ok(out)
}

fn resource_for_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Deployment" => "apps/v1/deployments",
        "StatefulSet" => "apps/v1/statefulsets",
        "DaemonSet" => "apps/v1/daemonsets",
        "ReplicaSet" => "apps/v1/replicasets",
        "Job" => "batch/v1/jobs",
        "CronJob" => "batch/v1/cronjobs",
        "Node" => "core/v1/nodes",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selector_reads_match_labels_or_flat_selector() {
        let deployment = json!({"spec": {"selector": {"matchLabels": {"app": "web"}}}});
        assert_eq!(
            selector_of(&deployment).get("app").map(String::as_str),
            Some("web")
        );

        let service = json!({"spec": {"selector": {"app": "web"}}});
        assert_eq!(
            selector_of(&service).get("app").map(String::as_str),
            Some("web")
        );
    }

    #[test]
    fn empty_selector_never_matches() {
        let labels: BTreeMap<String, String> = [("app".to_string(), "web".to_string())]
            .into_iter()
            .collect();
        assert!(!matches(&BTreeMap::new(), &labels));
    }

    #[test]
    fn every_selector_label_must_match() {
        let labels: BTreeMap<String, String> = [
            ("app".to_string(), "web".to_string()),
            ("tier".to_string(), "front".to_string()),
        ]
        .into_iter()
        .collect();

        let one: BTreeMap<String, String> = [("app".to_string(), "web".to_string())]
            .into_iter()
            .collect();
        assert!(matches(&one, &labels));

        let wrong: BTreeMap<String, String> = [
            ("app".to_string(), "web".to_string()),
            ("tier".to_string(), "back".to_string()),
        ]
        .into_iter()
        .collect();
        assert!(!matches(&wrong, &labels));
    }

    #[test]
    fn config_references_are_collected_from_every_shape() {
        let spec = json!({
            "containers": [{
                "envFrom": [
                    {"configMapRef": {"name": "shared-config"}},
                    {"secretRef": {"name": "shared-secret"}}
                ],
                "env": [
                    {"valueFrom": {"configMapKeyRef": {"name": "tuning"}}},
                    {"valueFrom": {"secretKeyRef": {"name": "db-password"}}}
                ]
            }],
            "volumes": [
                {"configMap": {"name": "nginx-conf"}},
                {"secret": {"secretName": "tls-cert"}},
                {"persistentVolumeClaim": {"claimName": "data"}},
                {"projected": {"sources": [{"secret": {"name": "token"}}]}}
            ]
        });

        let (config, claims) = referenced(Some(&spec));
        let names: Vec<&str> = config.iter().map(|(_, name)| name.as_str()).collect();
        assert!(names.contains(&"shared-config"));
        assert!(names.contains(&"shared-secret"));
        assert!(names.contains(&"tuning"));
        assert!(names.contains(&"db-password"));
        assert!(names.contains(&"nginx-conf"));
        assert!(names.contains(&"tls-cert"));
        assert!(
            names.contains(&"token"),
            "projected sources are references too"
        );
        assert_eq!(claims, vec!["data"]);
    }

    #[test]
    fn duplicate_references_are_collapsed() {
        let spec = json!({
            "containers": [
                {"envFrom": [{"configMapRef": {"name": "same"}}]},
                {"envFrom": [{"configMapRef": {"name": "same"}}]}
            ]
        });
        let (config, _) = referenced(Some(&spec));
        assert_eq!(config.len(), 1);
    }
}
