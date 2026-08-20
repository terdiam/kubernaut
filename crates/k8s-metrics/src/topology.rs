//! Topology: how traffic and ownership actually connect in a namespace.
//!
//! Built per namespace rather than cluster-wide on purpose. A graph of five
//! hundred pods is a hairball nobody can read; the useful question is almost
//! always "what does *this* namespace look like".

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::{
    apps::v1::ReplicaSet,
    core::v1::{Pod, Service},
    networking::v1::Ingress,
};
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};

/// Above this the graph stops being readable, so it is cut and flagged rather
/// than rendered as an unusable mass.
const MAX_NODES: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyNode {
    pub id: String,
    /// `Ingress` | `Service` | `Workload` | `Pod` | `Node`
    pub kind: String,
    /// Specific kind for workloads (`Deployment`, `StatefulSet`, …).
    pub sub_kind: Option<String>,
    pub name: String,
    pub namespace: Option<String>,
    /// `ok` | `pending` | `warning` | `error` | `unknown`
    pub health: String,
    /// One line of context shown under the label.
    pub detail: Option<String>,
    /// `group/version/plural`, so clicking a node can open it.
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyEdge {
    pub from: String,
    pub to: String,
    /// `routes` (ingress→service), `selects` (service→pod),
    /// `owns` (workload→pod), `runs` (pod→node)
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
    /// True when the graph was cut at [`MAX_NODES`].
    pub truncated: bool,
    pub namespaces: Vec<String>,
}

fn pod_health(pod: &Pod) -> String {
    let status = pod.status.as_ref();
    let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");

    if pod.metadata.deletion_timestamp.is_some() {
        return "warning".into();
    }
    for container in status
        .iter()
        .flat_map(|s| s.container_statuses.iter().flatten())
    {
        if container
            .state
            .as_ref()
            .and_then(|s| s.waiting.as_ref())
            .and_then(|w| w.reason.as_deref())
            .is_some_and(|reason| reason.ends_with("BackOff") || reason.ends_with("Error"))
        {
            return "error".into();
        }
    }
    match phase {
        "Running" => {
            let all_ready = status
                .and_then(|s| s.container_statuses.as_ref())
                .is_some_and(|list| !list.is_empty() && list.iter().all(|c| c.ready));
            if all_ready {
                "ok".into()
            } else {
                "pending".into()
            }
        }
        "Succeeded" => "ok".into(),
        "Pending" => "pending".into(),
        "Failed" => "error".into(),
        _ => "unknown".into(),
    }
}

/// Does a label selector map match a pod's labels?
fn selector_matches(selector: &BTreeMap<String, String>, pod: &Pod) -> bool {
    if selector.is_empty() {
        // An empty selector on a Service selects nothing (unlike an empty
        // selector elsewhere, which selects everything) — a Service with no
        // selector is manually managed through Endpoints.
        return false;
    }
    let labels = pod.labels();
    selector
        .iter()
        .all(|(key, value)| labels.get(key).map(String::as_str) == Some(value.as_str()))
}

/// Walk a pod's owner chain to the workload a person would recognise:
/// a ReplicaSet resolves to its Deployment, everything else stands for itself.
fn owning_workload(
    pod: &Pod,
    replica_sets: &HashMap<String, ReplicaSet>,
) -> Option<(String, String)> {
    let owner = pod
        .metadata
        .owner_references
        .as_ref()
        .and_then(|owners| owners.iter().find(|o| o.controller == Some(true)))?;

    if owner.kind == "ReplicaSet" {
        if let Some(rs) = replica_sets.get(&owner.name)
            && let Some(parent) = rs
                .metadata
                .owner_references
                .as_ref()
                .and_then(|owners| owners.iter().find(|o| o.controller == Some(true)))
        {
            return Some((parent.kind.clone(), parent.name.clone()));
        }
        // An orphaned ReplicaSet is worth showing as itself.
        return Some((owner.kind.clone(), owner.name.clone()));
    }
    Some((owner.kind.clone(), owner.name.clone()))
}

fn workload_resource(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Deployment" => "apps/v1/deployments",
        "StatefulSet" => "apps/v1/statefulsets",
        "DaemonSet" => "apps/v1/daemonsets",
        "ReplicaSet" => "apps/v1/replicasets",
        "Job" => "batch/v1/jobs",
        "CronJob" => "batch/v1/cronjobs",
        _ => return None,
    })
}

/// Build the graph for one or more namespaces.
pub async fn build(
    cluster: &Arc<ClusterHandle>,
    namespaces: &[String],
    pods_in_cluster: &[Pod],
) -> Result<Topology, String> {
    let wanted: HashSet<&str> = namespaces.iter().map(String::as_str).collect();
    let pods: Vec<&Pod> = pods_in_cluster
        .iter()
        .filter(|pod| {
            pod.namespace()
                .is_some_and(|ns| wanted.contains(ns.as_str()))
        })
        .collect();

    let mut nodes: Vec<TopologyNode> = Vec::new();
    let mut edges: Vec<TopologyEdge> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut truncated = false;

    let push = |node: TopologyNode, nodes: &mut Vec<TopologyNode>, seen: &mut HashSet<String>| {
        if seen.insert(node.id.clone()) {
            nodes.push(node);
        }
    };

    // Replica sets are only needed to bridge pods to their Deployment.
    let mut replica_sets: HashMap<String, ReplicaSet> = HashMap::new();
    for namespace in namespaces {
        let api: Api<ReplicaSet> = Api::namespaced(cluster.client.clone(), namespace);
        if let Ok(list) = api.list(&ListParams::default()).await {
            for rs in list {
                replica_sets.insert(rs.name_any(), rs);
            }
        }
    }

    for pod in &pods {
        let namespace = pod.namespace().unwrap_or_default();
        let name = pod.name_any();
        let id = format!("pod:{namespace}/{name}");

        push(
            TopologyNode {
                id: id.clone(),
                kind: "Pod".into(),
                sub_kind: None,
                name: name.clone(),
                namespace: Some(namespace.clone()),
                health: pod_health(pod),
                detail: pod.status.as_ref().and_then(|s| s.phase.clone()),
                resource: Some("core/v1/pods".into()),
            },
            &mut nodes,
            &mut seen,
        );

        if let Some(node_name) = pod.spec.as_ref().and_then(|s| s.node_name.clone()) {
            let node_id = format!("node:{node_name}");
            push(
                TopologyNode {
                    id: node_id.clone(),
                    kind: "Node".into(),
                    sub_kind: None,
                    name: node_name,
                    namespace: None,
                    health: "ok".into(),
                    detail: None,
                    resource: Some("core/v1/nodes".into()),
                },
                &mut nodes,
                &mut seen,
            );
            edges.push(TopologyEdge {
                from: id.clone(),
                to: node_id,
                kind: "runs".into(),
            });
        }

        if let Some((kind, workload_name)) = owning_workload(pod, &replica_sets) {
            let workload_id = format!("workload:{namespace}/{kind}/{workload_name}");
            push(
                TopologyNode {
                    id: workload_id.clone(),
                    kind: "Workload".into(),
                    sub_kind: Some(kind.clone()),
                    name: workload_name,
                    namespace: Some(namespace.clone()),
                    health: "unknown".into(),
                    detail: Some(kind.clone()),
                    resource: workload_resource(&kind).map(String::from),
                },
                &mut nodes,
                &mut seen,
            );
            edges.push(TopologyEdge {
                from: workload_id,
                to: id.clone(),
                kind: "owns".into(),
            });
        }
    }

    // Services, and which pods they select.
    let mut service_ids: HashMap<(String, String), String> = HashMap::new();
    for namespace in namespaces {
        let api: Api<Service> = Api::namespaced(cluster.client.clone(), namespace);
        let Ok(list) = api.list(&ListParams::default()).await else {
            continue;
        };
        for service in list {
            let name = service.name_any();
            let id = format!("service:{namespace}/{name}");
            let selector: BTreeMap<String, String> = service
                .spec
                .as_ref()
                .and_then(|s| s.selector.clone())
                .map(|s| s.into_iter().collect())
                .unwrap_or_default();

            let matched: Vec<&&Pod> = pods
                .iter()
                .filter(|pod| {
                    pod.namespace().as_deref() == Some(namespace.as_str())
                        && selector_matches(&selector, pod)
                })
                .collect();

            push(
                TopologyNode {
                    id: id.clone(),
                    kind: "Service".into(),
                    sub_kind: service.spec.as_ref().and_then(|s| s.type_.clone()),
                    name: name.clone(),
                    namespace: Some(namespace.clone()),
                    // A service selecting no pods is the classic silent
                    // misconfiguration, so it is called out rather than drawn
                    // like any other node.
                    health: if selector.is_empty() {
                        "unknown".into()
                    } else if matched.is_empty() {
                        "error".into()
                    } else {
                        "ok".into()
                    },
                    detail: Some(if selector.is_empty() {
                        "no selector".into()
                    } else {
                        format!("{} endpoint(s)", matched.len())
                    }),
                    resource: Some("core/v1/services".into()),
                },
                &mut nodes,
                &mut seen,
            );
            service_ids.insert((namespace.clone(), name), id.clone());

            for pod in matched {
                edges.push(TopologyEdge {
                    from: id.clone(),
                    to: format!(
                        "pod:{}/{}",
                        pod.namespace().unwrap_or_default(),
                        pod.name_any()
                    ),
                    kind: "selects".into(),
                });
            }
        }
    }

    // Ingresses on top.
    for namespace in namespaces {
        let api: Api<Ingress> = Api::namespaced(cluster.client.clone(), namespace);
        let Ok(list) = api.list(&ListParams::default()).await else {
            continue;
        };
        for ingress in list {
            let name = ingress.name_any();
            let id = format!("ingress:{namespace}/{name}");
            let hosts: Vec<String> = ingress
                .spec
                .as_ref()
                .and_then(|s| s.rules.as_ref())
                .map(|rules| rules.iter().filter_map(|r| r.host.clone()).collect())
                .unwrap_or_default();

            push(
                TopologyNode {
                    id: id.clone(),
                    kind: "Ingress".into(),
                    sub_kind: ingress
                        .spec
                        .as_ref()
                        .and_then(|s| s.ingress_class_name.clone()),
                    name: name.clone(),
                    namespace: Some(namespace.clone()),
                    health: "ok".into(),
                    detail: Some(if hosts.is_empty() {
                        "*".into()
                    } else {
                        hosts.join(", ")
                    }),
                    resource: Some("networking.k8s.io/v1/ingresses".into()),
                },
                &mut nodes,
                &mut seen,
            );

            let backends = ingress
                .spec
                .as_ref()
                .map(|spec| {
                    let mut names: Vec<String> = Vec::new();
                    if let Some(default) = spec
                        .default_backend
                        .as_ref()
                        .and_then(|b| b.service.as_ref())
                    {
                        names.push(default.name.clone());
                    }
                    for rule in spec.rules.iter().flatten() {
                        for path in rule.http.iter().flat_map(|h| h.paths.iter()) {
                            if let Some(service) = path.backend.service.as_ref() {
                                names.push(service.name.clone());
                            }
                        }
                    }
                    names
                })
                .unwrap_or_default();

            for backend in backends {
                if let Some(service_id) = service_ids.get(&(namespace.clone(), backend.clone())) {
                    edges.push(TopologyEdge {
                        from: id.clone(),
                        to: service_id.clone(),
                        kind: "routes".into(),
                    });
                } else {
                    // A route to a service that does not exist is worth seeing.
                    let missing = format!("service:{namespace}/{backend}");
                    push(
                        TopologyNode {
                            id: missing.clone(),
                            kind: "Service".into(),
                            sub_kind: None,
                            name: backend,
                            namespace: Some(namespace.clone()),
                            health: "error".into(),
                            detail: Some("missing".into()),
                            resource: Some("core/v1/services".into()),
                        },
                        &mut nodes,
                        &mut seen,
                    );
                    edges.push(TopologyEdge {
                        from: id.clone(),
                        to: missing,
                        kind: "routes".into(),
                    });
                }
            }
        }
    }

    if nodes.len() > MAX_NODES {
        truncated = true;
        let kept: HashSet<String> = nodes.iter().take(MAX_NODES).map(|n| n.id.clone()).collect();
        nodes.truncate(MAX_NODES);
        edges.retain(|edge| kept.contains(&edge.from) && kept.contains(&edge.to));
    }

    Ok(Topology {
        nodes,
        edges,
        truncated,
        namespaces: namespaces.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::PodStatus;
    use kube::core::ObjectMeta;

    fn pod_with_labels(labels: &[(&str, &str)]) -> Pod {
        Pod {
            metadata: ObjectMeta {
                name: Some("web-1".into()),
                namespace: Some("default".into()),
                labels: Some(
                    labels
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn selector_needs_every_label_to_match() {
        let pod = pod_with_labels(&[("app", "web"), ("tier", "frontend")]);
        let mut selector = BTreeMap::new();
        selector.insert("app".to_string(), "web".to_string());
        assert!(selector_matches(&selector, &pod));

        selector.insert("tier".to_string(), "backend".to_string());
        assert!(!selector_matches(&selector, &pod), "one mismatch is enough");
    }

    /// A Service with no selector is managed through Endpoints by hand; it must
    /// not be treated as selecting every pod in the namespace.
    #[test]
    fn empty_selector_selects_nothing() {
        let pod = pod_with_labels(&[("app", "web")]);
        assert!(!selector_matches(&BTreeMap::new(), &pod));
    }

    #[test]
    fn replica_set_owner_resolves_to_the_deployment() {
        let mut pod = pod_with_labels(&[]);
        pod.metadata.owner_references = Some(vec![
            k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
                kind: "ReplicaSet".into(),
                name: "web-5f8c".into(),
                controller: Some(true),
                ..Default::default()
            },
        ]);

        let mut replica_sets = HashMap::new();
        replica_sets.insert(
            "web-5f8c".to_string(),
            ReplicaSet {
                metadata: ObjectMeta {
                    name: Some("web-5f8c".into()),
                    owner_references: Some(vec![
                        k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
                            kind: "Deployment".into(),
                            name: "web".into(),
                            controller: Some(true),
                            ..Default::default()
                        },
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(
            owning_workload(&pod, &replica_sets),
            Some(("Deployment".into(), "web".into()))
        );
    }

    #[test]
    fn orphaned_replica_set_stands_for_itself() {
        let mut pod = pod_with_labels(&[]);
        pod.metadata.owner_references = Some(vec![
            k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
                kind: "ReplicaSet".into(),
                name: "orphan".into(),
                controller: Some(true),
                ..Default::default()
            },
        ]);
        assert_eq!(
            owning_workload(&pod, &HashMap::new()),
            Some(("ReplicaSet".into(), "orphan".into()))
        );
    }

    #[test]
    fn crashlooping_pod_is_unhealthy() {
        let pod = Pod {
            status: Some(PodStatus {
                phase: Some("Running".into()),
                container_statuses: Some(vec![k8s_openapi::api::core::v1::ContainerStatus {
                    name: "web".into(),
                    ready: false,
                    state: Some(k8s_openapi::api::core::v1::ContainerState {
                        waiting: Some(k8s_openapi::api::core::v1::ContainerStateWaiting {
                            reason: Some("CrashLoopBackOff".into()),
                            message: None,
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(pod_health(&pod), "error");
    }
}
