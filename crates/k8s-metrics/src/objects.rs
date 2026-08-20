//! Metrics for one object: a pod, node, namespace or workload.
//!
//! Two sources, in order of preference:
//!
//! * **Prometheus**, when the cluster has one — a range query returns real
//!   history immediately, including time before the app was opened.
//! * **metrics-server**, otherwise — sampled here and accumulated into a small
//!   ring per target, so a chart starts empty and fills as the app runs.
//!
//! The ring is created on first request and dropped once nothing has asked for
//! that target in a while, so watching one pod does not cost memory for the
//! other five hundred.

use serde::{Deserialize, Serialize};

/// What to chart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MetricTarget {
    Pod {
        namespace: String,
        name: String,
    },
    Node {
        name: String,
    },
    Namespace {
        name: String,
    },
    /// Every pod behind a workload, aggregated.
    Workload {
        namespace: String,
        /// `group/version/plural`, used to resolve the pod selector.
        resource: String,
        name: String,
    },
}

impl MetricTarget {
    /// Stable key for the per-target ring.
    pub fn key(&self) -> String {
        match self {
            Self::Pod { namespace, name } => format!("pod/{namespace}/{name}"),
            Self::Node { name } => format!("node/{name}"),
            Self::Namespace { name } => format!("ns/{name}"),
            Self::Workload {
                namespace,
                resource,
                name,
            } => format!("workload/{namespace}/{resource}/{name}"),
        }
    }
}

/// A chartable point.
///
/// Network figures are bytes per second (already differentiated from the
/// kubelet's cumulative counters); filesystem figures are absolute bytes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub at: i64,
    pub cpu: f64,
    pub memory: f64,
    #[serde(default)]
    pub network_rx: f64,
    #[serde(default)]
    pub network_tx: f64,
    /// Container writable layer plus logs.
    #[serde(default)]
    pub fs_used: f64,
    /// Mounted volumes, summed.
    #[serde(default)]
    pub volume_used: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetricSource {
    Prometheus,
    MetricsServer,
    /// Neither is available; only requests and limits are known.
    None,
}

/// Everything the object metrics panel draws.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMetrics {
    pub source: MetricSource,
    pub points: Vec<Point>,
    /// Declared on the object; drawn as reference lines on the chart.
    pub cpu_requests: f64,
    pub cpu_limits: f64,
    pub memory_requests: f64,
    pub memory_limits: f64,
    /// Number of pods the figures cover, for workloads and namespaces.
    pub pod_count: usize,
    /// Network and filesystem series, from the kubelet.
    ///
    /// Kept separate from `points` rather than merged: CPU and memory may come
    /// from Prometheus with its own timestamps, while I/O always comes from
    /// this session's kubelet sampling. Interleaving two timelines into one
    /// series would invent points that were never measured.
    #[serde(default)]
    pub io_points: Vec<Point>,
    /// Why I/O stats are missing, when they are.
    pub io_note: Option<String>,
    /// Set when history is unavailable and why.
    pub note: Option<String>,
}

/// Usage and declared resources for one namespace — a heatmap row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceUsage {
    pub namespace: String,
    pub pods: usize,
    pub cpu_usage: f64,
    pub cpu_requests: f64,
    pub cpu_limits: f64,
    pub memory_usage: f64,
    pub memory_requests: f64,
    pub memory_limits: f64,
    /// True when at least one container in the namespace declares no request,
    /// which makes the usage/request ratio meaningless for that row.
    pub has_unset_requests: bool,
}

/// Live figures for one node, merged from the node object and metrics.
///
/// The node object knows what the machine has; `metrics.k8s.io` knows what is
/// being used; the pod store knows how many slots are taken. None of the three
/// is useful alone, and a node list that shows capacity without usage answers
/// the wrong question.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSummary {
    pub name: String,
    /// Cores.
    pub cpu_usage: f64,
    pub cpu_requests: f64,
    pub cpu_allocatable: f64,
    pub cpu_capacity: f64,
    /// Bytes.
    pub memory_usage: f64,
    pub memory_requests: f64,
    pub memory_allocatable: f64,
    pub memory_capacity: f64,
    /// Pod slots.
    pub pods_used: f64,
    pub pods_allocatable: f64,
    /// Bytes on the filesystem the kubelet writes to — the one that triggers
    /// disk-pressure eviction when it fills.
    pub disk_used: f64,
    pub disk_capacity: f64,
    /// Image filesystem, when the runtime uses a separate device.
    pub image_disk_used: f64,
    pub image_disk_capacity: f64,
    /// False when metrics-server did not report this node.
    pub usage_available: bool,
    /// False when the kubelet summary could not be read (usually RBAC).
    pub disk_available: bool,

    pub os_image: Option<String>,
    pub kernel_version: Option<String>,
    pub container_runtime: Option<String>,
    pub kubelet_version: Option<String>,
    pub architecture: Option<String>,
    pub operating_system: Option<String>,
}
