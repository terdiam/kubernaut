//! Aggregation: nodes and pods in, one overview out.

use std::collections::{HashMap, HashSet};

use k8s_openapi::api::core::v1::{Node, Pod, PodSpec};
use kube::ResourceExt;

use crate::{
    model::{ClusterOverview, Issue, NodeCounts, NodeScope, ResourceGauge, Severity},
    quantity,
};

/// Label families that mark a control-plane node. Both are in the wild: the
/// `master` form predates the rename and is still used by several installers.
const CONTROL_PLANE_LABELS: [&str; 2] = [
    "node-role.kubernetes.io/control-plane",
    "node-role.kubernetes.io/master",
];

pub fn is_control_plane(node: &Node) -> bool {
    let labels = node.labels();
    CONTROL_PLANE_LABELS.iter().any(|l| labels.contains_key(*l))
}

pub fn in_scope(node: &Node, scope: NodeScope) -> bool {
    match scope {
        NodeScope::All => true,
        NodeScope::ControlPlane => is_control_plane(node),
        NodeScope::Workers => !is_control_plane(node),
    }
}

fn node_ready(node: &Node) -> Option<bool> {
    node.status
        .as_ref()?
        .conditions
        .as_ref()?
        .iter()
        .find(|c| c.type_ == "Ready")
        .map(|c| c.status == "True")
}

/// Effective resource request/limit for a pod.
///
/// Kubernetes does not simply add every container: init containers run to
/// completion before app containers start, so a pod's effective request is
/// `max(sum(app containers), max(init containers))`. Summing everything
/// overstates large init containers, sometimes badly.
fn pod_resources(spec: &PodSpec, key: &str) -> (f64, f64) {
    let mut request_sum = 0.0;
    let mut limit_sum = 0.0;
    for container in &spec.containers {
        if let Some(resources) = &container.resources {
            request_sum += resources
                .requests
                .as_ref()
                .and_then(|r| r.get(key))
                .and_then(|q| quantity::parse(&q.0))
                .unwrap_or(0.0);
            limit_sum += resources
                .limits
                .as_ref()
                .and_then(|r| r.get(key))
                .and_then(|q| quantity::parse(&q.0))
                .unwrap_or(0.0);
        }
    }

    let mut request_max: f64 = 0.0;
    let mut limit_max: f64 = 0.0;
    for container in spec.init_containers.iter().flatten() {
        if let Some(resources) = &container.resources {
            request_max = request_max.max(
                resources
                    .requests
                    .as_ref()
                    .and_then(|r| r.get(key))
                    .and_then(|q| quantity::parse(&q.0))
                    .unwrap_or(0.0),
            );
            limit_max = limit_max.max(
                resources
                    .limits
                    .as_ref()
                    .and_then(|r| r.get(key))
                    .and_then(|q| quantity::parse(&q.0))
                    .unwrap_or(0.0),
            );
        }
    }

    (request_sum.max(request_max), limit_sum.max(limit_max))
}

/// Pods that still occupy a slot on a node. Succeeded/Failed pods have released
/// their resources, so counting them would inflate every gauge.
fn occupies_node(pod: &Pod) -> bool {
    !matches!(
        pod.status.as_ref().and_then(|s| s.phase.as_deref()),
        Some("Succeeded") | Some("Failed")
    )
}

/// Effective pod request/limit for a resource key, exposed for the per-object
/// metrics panel so both paths agree on the init-container rule.
pub fn pod_resources_public(spec: &PodSpec, key: &str) -> (f64, f64) {
    pod_resources(spec, key)
}

/// Node usage sampled from `metrics.k8s.io`, keyed by node name.
#[derive(Debug, Default, Clone)]
pub struct NodeUsage {
    pub cpu: HashMap<String, f64>,
    pub memory: HashMap<String, f64>,
}

pub fn build(
    nodes: &[Node],
    pods: &[Pod],
    usage: Option<&NodeUsage>,
    metrics_error: Option<String>,
    scope: NodeScope,
    sampled_at: String,
) -> ClusterOverview {
    let scoped: Vec<&Node> = nodes.iter().filter(|n| in_scope(n, scope)).collect();
    let names: HashSet<&str> = scoped
        .iter()
        .map(|n| n.metadata.name.as_deref().unwrap_or(""))
        .collect();

    let mut cpu = ResourceGauge::default();
    let mut memory = ResourceGauge::default();
    let mut pod_slots = ResourceGauge::default();

    for node in &scoped {
        if let Some(status) = &node.status {
            if let Some(capacity) = &status.capacity {
                cpu.capacity += quantity::parse_or_zero(capacity.get("cpu"));
                memory.capacity += quantity::parse_or_zero(capacity.get("memory"));
                pod_slots.capacity += quantity::parse_or_zero(capacity.get("pods"));
            }
            if let Some(allocatable) = &status.allocatable {
                cpu.allocatable += quantity::parse_or_zero(allocatable.get("cpu"));
                memory.allocatable += quantity::parse_or_zero(allocatable.get("memory"));
                pod_slots.allocatable += quantity::parse_or_zero(allocatable.get("pods"));
            }
        }
    }

    let mut counted_pods = 0.0;
    for pod in pods {
        let node_name = pod.spec.as_ref().and_then(|s| s.node_name.as_deref());
        // Unscheduled pods belong to no node, so they cannot be attributed to a
        // scope; they surface in the issues list instead.
        let Some(node_name) = node_name else { continue };
        if !names.contains(node_name) || !occupies_node(pod) {
            continue;
        }
        counted_pods += 1.0;
        if let Some(spec) = &pod.spec {
            let (cpu_request, cpu_limit) = pod_resources(spec, "cpu");
            let (memory_request, memory_limit) = pod_resources(spec, "memory");
            cpu.requests += cpu_request;
            cpu.limits += cpu_limit;
            memory.requests += memory_request;
            memory.limits += memory_limit;
        }
    }
    pod_slots.usage = counted_pods;
    pod_slots.usage_available = true;

    if let Some(usage) = usage {
        for node in &scoped {
            let name = node.name_any();
            cpu.usage += usage.cpu.get(&name).copied().unwrap_or(0.0);
            memory.usage += usage.memory.get(&name).copied().unwrap_or(0.0);
        }
        cpu.usage_available = true;
        memory.usage_available = true;
    }

    let counts = NodeCounts {
        total: scoped.len(),
        ready: scoped
            .iter()
            .filter(|n| node_ready(n) == Some(true))
            .count(),
        not_ready: scoped
            .iter()
            .filter(|n| node_ready(n) != Some(true))
            .count(),
        unschedulable: scoped
            .iter()
            .filter(|n| n.spec.as_ref().and_then(|s| s.unschedulable) == Some(true))
            .count(),
    };

    ClusterOverview {
        scope,
        nodes: counts,
        cpu,
        memory,
        pods: pod_slots,
        issues: issues(&scoped, pods, &names),
        sampled_at,
        metrics_available: usage.is_some(),
        metrics_error,
    }
}

/// Problems worth interrupting someone for. Deliberately conservative: a list
/// that cries wolf gets ignored, and then the real outage hides in it.
fn issues(nodes: &[&Node], pods: &[Pod], scoped_names: &HashSet<&str>) -> Vec<Issue> {
    let mut out = Vec::new();

    for node in nodes {
        let name = node.name_any();
        match node_ready(node) {
            Some(true) => {}
            Some(false) => out.push(Issue {
                severity: Severity::Error,
                kind: "Node".into(),
                resource: "core/v1/nodes".into(),
                namespace: None,
                name: name.clone(),
                message: "Node is NotReady".into(),
            }),
            None => out.push(Issue {
                severity: Severity::Warning,
                kind: "Node".into(),
                resource: "core/v1/nodes".into(),
                namespace: None,
                name: name.clone(),
                message: "Node has not reported a Ready condition".into(),
            }),
        }
        if node.spec.as_ref().and_then(|s| s.unschedulable) == Some(true) {
            out.push(Issue {
                severity: Severity::Warning,
                kind: "Node".into(),
                resource: "core/v1/nodes".into(),
                namespace: None,
                name,
                message: "Node is cordoned; the scheduler will not place pods on it".into(),
            });
        }
    }

    for pod in pods {
        let scheduled_here = pod
            .spec
            .as_ref()
            .and_then(|s| s.node_name.as_deref())
            .is_some_and(|n| scoped_names.contains(n));
        let unscheduled = pod
            .spec
            .as_ref()
            .and_then(|s| s.node_name.as_deref())
            .is_none();
        if !scheduled_here && !unscheduled {
            continue;
        }

        let name = pod.name_any();
        let namespace = pod.namespace();
        let status = pod.status.as_ref();
        let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");

        if phase == "Failed" {
            out.push(Issue {
                severity: Severity::Error,
                kind: "Pod".into(),
                resource: "core/v1/pods".into(),
                namespace: namespace.clone(),
                name: name.clone(),
                message: status
                    .and_then(|s| s.reason.clone())
                    .unwrap_or_else(|| "Pod failed".into()),
            });
            continue;
        }

        if unscheduled && phase == "Pending" {
            let reason = status
                .and_then(|s| s.conditions.as_ref())
                .and_then(|c| c.iter().find(|c| c.type_ == "PodScheduled"))
                .and_then(|c| c.message.clone())
                .unwrap_or_else(|| "Pod is pending and not scheduled to a node".into());
            out.push(Issue {
                severity: Severity::Warning,
                kind: "Pod".into(),
                resource: "core/v1/pods".into(),
                namespace: namespace.clone(),
                name: name.clone(),
                message: reason,
            });
            continue;
        }

        let statuses = status
            .map(|s| {
                s.container_statuses
                    .iter()
                    .flatten()
                    .chain(s.init_container_statuses.iter().flatten())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for container in statuses {
            let waiting_reason = container
                .state
                .as_ref()
                .and_then(|s| s.waiting.as_ref())
                .and_then(|w| w.reason.as_deref());
            if let Some(reason) = waiting_reason
                && matches!(
                    reason,
                    "CrashLoopBackOff"
                        | "ImagePullBackOff"
                        | "ErrImagePull"
                        | "CreateContainerConfigError"
                        | "CreateContainerError"
                        | "InvalidImageName"
                )
            {
                out.push(Issue {
                    severity: Severity::Error,
                    kind: "Pod".into(),
                    resource: "core/v1/pods".into(),
                    namespace: namespace.clone(),
                    name: name.clone(),
                    message: format!("{}: {reason}", container.name),
                });
            }
        }
    }

    // Errors first, then alphabetically, so the list is stable between samples
    // and does not shuffle under the reader.
    out.sort_by(|a, b| {
        let rank = |s: &Severity| match s {
            Severity::Error => 0,
            Severity::Warning => 1,
        };
        rank(&a.severity)
            .cmp(&rank(&b.severity))
            .then_with(|| a.namespace.cmp(&b.namespace))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.message.cmp(&b.message))
    });
    out.dedup_by(|a, b| a.name == b.name && a.message == b.message && a.namespace == b.namespace);
    out
}

/// Per-node usage, capacity and system information.
pub fn node_summaries(
    nodes: &[Node],
    pods: &[Pod],
    usage: Option<&NodeUsage>,
    disk: &std::collections::HashMap<String, crate::kubelet::NodeFilesystem>,
) -> Vec<crate::objects::NodeSummary> {
    use crate::objects::NodeSummary;

    let mut out: Vec<NodeSummary> = nodes
        .iter()
        .map(|node| {
            let name = node.name_any();
            let status = node.status.as_ref();
            let capacity = status.and_then(|s| s.capacity.as_ref());
            let allocatable = status.and_then(|s| s.allocatable.as_ref());
            let info = status.and_then(|s| s.node_info.as_ref());

            let mut cpu_requests = 0.0;
            let mut memory_requests = 0.0;
            let mut pods_used = 0.0;
            for pod in pods {
                if pod.spec.as_ref().and_then(|s| s.node_name.as_deref()) != Some(name.as_str())
                    || !occupies_node(pod)
                {
                    continue;
                }
                pods_used += 1.0;
                if let Some(spec) = &pod.spec {
                    cpu_requests += pod_resources(spec, "cpu").0;
                    memory_requests += pod_resources(spec, "memory").0;
                }
            }

            let filesystem = disk.get(&name).copied();
            let (cpu_usage, memory_usage, usage_available) = match usage {
                Some(usage) => (
                    usage.cpu.get(&name).copied().unwrap_or(0.0),
                    usage.memory.get(&name).copied().unwrap_or(0.0),
                    usage.cpu.contains_key(&name),
                ),
                None => (0.0, 0.0, false),
            };

            NodeSummary {
                name,
                cpu_usage,
                cpu_requests,
                cpu_allocatable: quantity::parse_or_zero(allocatable.and_then(|a| a.get("cpu"))),
                cpu_capacity: quantity::parse_or_zero(capacity.and_then(|c| c.get("cpu"))),
                memory_usage,
                memory_requests,
                memory_allocatable: quantity::parse_or_zero(
                    allocatable.and_then(|a| a.get("memory")),
                ),
                memory_capacity: quantity::parse_or_zero(capacity.and_then(|c| c.get("memory"))),
                pods_used,
                pods_allocatable: quantity::parse_or_zero(allocatable.and_then(|a| a.get("pods"))),
                disk_used: filesystem.map(|fs| fs.used_bytes).unwrap_or(0.0),
                disk_capacity: filesystem.map(|fs| fs.capacity_bytes).unwrap_or(0.0),
                image_disk_used: filesystem.map(|fs| fs.image_used_bytes).unwrap_or(0.0),
                image_disk_capacity: filesystem.map(|fs| fs.image_capacity_bytes).unwrap_or(0.0),
                usage_available,
                disk_available: filesystem.is_some_and(|fs| fs.capacity_bytes > 0.0),
                os_image: info.map(|i| i.os_image.clone()),
                kernel_version: info.map(|i| i.kernel_version.clone()),
                container_runtime: info.map(|i| i.container_runtime_version.clone()),
                kubelet_version: info.map(|i| i.kubelet_version.clone()),
                architecture: info.map(|i| i.architecture.clone()),
                operating_system: info.map(|i| i.operating_system.clone()),
            }
        })
        .collect();

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Per-namespace usage against declared requests and limits — the heatmap.
///
/// `has_unset_requests` matters: a namespace where containers declare nothing
/// shows a usage/request ratio of infinity, which is not "over budget" but
/// "no budget was set". The two need to read differently.
pub fn namespace_usage(
    pods: &[Pod],
    usage: &std::collections::HashMap<String, (f64, f64)>,
) -> Vec<crate::objects::NamespaceUsage> {
    use std::collections::HashMap;

    struct Accumulator {
        pods: usize,
        cpu_usage: f64,
        cpu_requests: f64,
        cpu_limits: f64,
        memory_usage: f64,
        memory_requests: f64,
        memory_limits: f64,
        unset: bool,
    }

    let mut by_namespace: HashMap<String, Accumulator> = HashMap::new();

    for pod in pods {
        if !occupies_node(pod) {
            continue;
        }
        let namespace = pod.namespace().unwrap_or_default();
        let name = pod.name_any();
        let entry = by_namespace
            .entry(namespace.clone())
            .or_insert(Accumulator {
                pods: 0,
                cpu_usage: 0.0,
                cpu_requests: 0.0,
                cpu_limits: 0.0,
                memory_usage: 0.0,
                memory_requests: 0.0,
                memory_limits: 0.0,
                unset: false,
            });

        entry.pods += 1;
        if let Some((cpu, memory)) = usage.get(&format!("{namespace}/{name}")) {
            entry.cpu_usage += cpu;
            entry.memory_usage += memory;
        }
        if let Some(spec) = &pod.spec {
            let (cpu_request, cpu_limit) = pod_resources(spec, "cpu");
            let (memory_request, memory_limit) = pod_resources(spec, "memory");
            entry.cpu_requests += cpu_request;
            entry.cpu_limits += cpu_limit;
            entry.memory_requests += memory_request;
            entry.memory_limits += memory_limit;
            if cpu_request == 0.0 || memory_request == 0.0 {
                entry.unset = true;
            }
        }
    }

    let mut out: Vec<crate::objects::NamespaceUsage> = by_namespace
        .into_iter()
        .map(|(namespace, acc)| crate::objects::NamespaceUsage {
            namespace,
            pods: acc.pods,
            cpu_usage: acc.cpu_usage,
            cpu_requests: acc.cpu_requests,
            cpu_limits: acc.cpu_limits,
            memory_usage: acc.memory_usage,
            memory_requests: acc.memory_requests,
            memory_limits: acc.memory_limits,
            has_unset_requests: acc.unset,
        })
        .collect();

    out.sort_by(|a, b| {
        b.cpu_usage
            .partial_cmp(&a.cpu_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.namespace.cmp(&b.namespace))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{Container, ResourceRequirements};
    use std::collections::BTreeMap;

    fn quantity_map(
        pairs: &[(&str, &str)],
    ) -> BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    k8s_openapi::apimachinery::pkg::api::resource::Quantity((*v).to_string()),
                )
            })
            .collect()
    }

    fn container(name: &str, cpu_request: &str, cpu_limit: &str) -> Container {
        Container {
            name: name.into(),
            resources: Some(ResourceRequirements {
                requests: Some(quantity_map(&[("cpu", cpu_request)])),
                limits: Some(quantity_map(&[("cpu", cpu_limit)])),
                claims: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn app_container_requests_are_summed() {
        let spec = PodSpec {
            containers: vec![
                container("a", "100m", "200m"),
                container("b", "250m", "500m"),
            ],
            ..Default::default()
        };
        let (requests, limits) = pod_resources(&spec, "cpu");
        assert!((requests - 0.35).abs() < 1e-9);
        assert!((limits - 0.7).abs() < 1e-9);
    }

    /// Init containers run before the app containers, so they take the max, not
    /// the sum — this is the rule that makes a big migration job's init
    /// container not double-count against the workload.
    #[test]
    fn init_containers_take_the_max_not_the_sum() {
        let spec = PodSpec {
            containers: vec![container("app", "100m", "100m")],
            init_containers: Some(vec![
                container("init-a", "2", "2"),
                container("init-b", "1", "1"),
            ]),
            ..Default::default()
        };
        let (requests, _) = pod_resources(&spec, "cpu");
        assert_eq!(requests, 2.0, "max(init)=2 beats sum(app)=0.1");
    }

    fn pod_in_phase(phase: &str) -> Pod {
        Pod {
            status: Some(k8s_openapi::api::core::v1::PodStatus {
                phase: Some(phase.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn finished_pods_do_not_occupy_a_node() {
        assert!(!occupies_node(&pod_in_phase("Succeeded")));
        assert!(!occupies_node(&pod_in_phase("Failed")));
        assert!(occupies_node(&pod_in_phase("Running")));
        assert!(occupies_node(&pod_in_phase("Pending")));
    }

    #[test]
    fn control_plane_is_detected_from_either_label() {
        let node = Node {
            metadata: kube::core::ObjectMeta {
                labels: Some(
                    [("node-role.kubernetes.io/master".to_string(), String::new())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(is_control_plane(&node));
        assert!(in_scope(&node, NodeScope::ControlPlane));
        assert!(!in_scope(&node, NodeScope::Workers));
        assert!(in_scope(&node, NodeScope::All));
    }

    #[test]
    fn crashlooping_container_becomes_an_error() {
        let pod = Pod {
            metadata: kube::core::ObjectMeta {
                name: Some("web".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                node_name: Some("node1".into()),
                ..Default::default()
            }),
            status: Some(k8s_openapi::api::core::v1::PodStatus {
                phase: Some("Running".into()),
                container_statuses: Some(vec![k8s_openapi::api::core::v1::ContainerStatus {
                    name: "web".into(),
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
        };

        let names: HashSet<&str> = ["node1"].into_iter().collect();
        let found = issues(&[], std::slice::from_ref(&pod), &names);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, Severity::Error);
        assert!(found[0].message.contains("CrashLoopBackOff"));
        // The overview lists issues for objects nothing is watching yet, so the
        // row has to carry its own resource key to be openable.
        assert_eq!(found[0].resource, "core/v1/pods");
    }

    #[test]
    fn node_issues_are_navigable() {
        let node = Node {
            metadata: kube::core::ObjectMeta {
                name: Some("node1".into()),
                ..Default::default()
            },
            status: Some(k8s_openapi::api::core::v1::NodeStatus {
                conditions: Some(vec![k8s_openapi::api::core::v1::NodeCondition {
                    type_: "Ready".into(),
                    status: "False".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let names: HashSet<&str> = ["node1"].into_iter().collect();
        let found = issues(&[&node], &[], &names);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].resource, "core/v1/nodes");
        assert!(found[0].namespace.is_none());
    }

    #[test]
    fn healthy_cluster_reports_no_issues() {
        let node = Node {
            metadata: kube::core::ObjectMeta {
                name: Some("node1".into()),
                ..Default::default()
            },
            status: Some(k8s_openapi::api::core::v1::NodeStatus {
                conditions: Some(vec![k8s_openapi::api::core::v1::NodeCondition {
                    type_: "Ready".into(),
                    status: "True".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let names: HashSet<&str> = ["node1"].into_iter().collect();
        assert!(issues(&[&node], &[], &names).is_empty());
    }
}
