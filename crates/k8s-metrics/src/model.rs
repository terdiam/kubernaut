//! Shapes the overview screen renders.

use k8s_openapi::jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Which nodes an overview covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub enum NodeScope {
    All,
    /// Nodes carrying a control-plane/master role label.
    ControlPlane,
    /// Everything else — where workloads actually run.
    Workers,
}

impl NodeScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "All Nodes",
            Self::ControlPlane => "Control Plane",
            Self::Workers => "Worker Nodes",
        }
    }
}

/// Usage against what is requested, limited, allocatable and physically present.
///
/// The five numbers answer different questions and are routinely conflated:
/// `capacity` is what the machine has, `allocatable` is what the scheduler may
/// hand out (capacity minus reserved), `requests` is what is promised to pods,
/// `limits` is the ceiling, and `usage` is what is actually consumed now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGauge {
    pub usage: f64,
    pub requests: f64,
    pub limits: f64,
    pub allocatable: f64,
    pub capacity: f64,
    /// False when metrics-server is absent, so the UI can say so instead of
    /// drawing a confident zero.
    pub usage_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Warning,
    Error,
}

/// One thing wrong in the cluster, as shown in the issues panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub severity: Severity,
    /// Kind of the object the issue is about, for the label.
    pub kind: String,
    /// `group/version/plural`, so the panel can open the object rather than
    /// only naming it.
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeCounts {
    pub total: usize,
    pub ready: usize,
    pub not_ready: usize,
    pub unschedulable: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterOverview {
    pub scope: NodeScope,
    pub nodes: NodeCounts,
    /// CPU in cores.
    pub cpu: ResourceGauge,
    /// Memory in bytes.
    pub memory: ResourceGauge,
    /// Pod slots.
    pub pods: ResourceGauge,
    pub issues: Vec<Issue>,
    pub sampled_at: String,
    /// False when `metrics.k8s.io` is unavailable.
    pub metrics_available: bool,
    /// Populated when the metrics API failed, so the UI can explain why.
    pub metrics_error: Option<String>,
}

/// One point in the usage history.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// Milliseconds since the epoch — what the chart plots on x.
    pub at: i64,
    pub cpu_usage: f64,
    pub cpu_requests: f64,
    pub cpu_limits: f64,
    pub memory_usage: f64,
    pub memory_requests: f64,
    pub memory_limits: f64,
    pub pods: f64,
}

impl Sample {
    pub fn new(at: Timestamp, overview: &ClusterOverview) -> Self {
        Self {
            at: at.as_millisecond(),
            cpu_usage: overview.cpu.usage,
            cpu_requests: overview.cpu.requests,
            cpu_limits: overview.cpu.limits,
            memory_usage: overview.memory.usage,
            memory_requests: overview.memory.requests,
            memory_limits: overview.memory.limits,
            pods: overview.pods.usage,
        }
    }
}
