//! Turning a [`MetricTarget`] into a chart: pick the best source, gather the
//! declared requests and limits, and return points.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Pod;
use kube::ResourceExt;

use crate::{
    objects::{MetricSource, MetricTarget, ObjectMetrics, Point},
    overview,
    prometheus::{self, queries},
    sampler::ClusterSampler,
};

/// Fetch metrics for one target over `window_ms` of history.
pub async fn fetch(
    cluster: &Arc<ClusterHandle>,
    sampler: &Arc<ClusterSampler>,
    target: &MetricTarget,
    window_ms: i64,
) -> ObjectMetrics {
    let pods = sampler.pods();
    let matching = pods_for_target(cluster, &pods, target).await;

    let (cpu_requests, cpu_limits, memory_requests, memory_limits) = declared(&matching);

    // Network and disk are only attributable for a single pod or a node.
    let io_capable = matches!(target, MetricTarget::Pod { .. } | MetricTarget::Node { .. });
    // Registering the target here is also what starts kubelet sampling for it.
    let local = sampler.target_history(target);
    let io_points: Vec<Point> = if io_capable {
        local
            .iter()
            .copied()
            .filter(|point| point.fs_used > 0.0 || point.network_rx > 0.0 || point.network_tx > 0.0)
            .collect()
    } else {
        Vec::new()
    };

    // Prometheus first for CPU and memory: it has history from before the app
    // was opened.
    if let Some(promql_target) = sampler.prometheus() {
        match prometheus_points(cluster, &promql_target, target, &matching, window_ms).await {
            Ok(points) if !points.is_empty() => {
                return ObjectMetrics {
                    source: MetricSource::Prometheus,
                    points,
                    cpu_requests,
                    cpu_limits,
                    memory_requests,
                    memory_limits,
                    pod_count: matching.len(),
                    io_points,
                    io_note: io_note(sampler, io_capable),
                    note: None,
                };
            }
            Ok(_) => {}
            Err(err) => tracing::debug!(%err, "prometheus range query failed; falling back"),
        }
    }

    // metrics-server: whatever this session has accumulated.
    let points = local;
    let note = if points.is_empty() {
        Some(
            if sampler.prometheus_checked() && sampler.prometheus().is_none() {
                "No Prometheus in this cluster, so history starts now and fills as the app runs."
                    .to_string()
            } else {
                "Collecting the first samples…".to_string()
            },
        )
    } else {
        None
    };

    ObjectMetrics {
        source: if points.is_empty() {
            MetricSource::None
        } else {
            MetricSource::MetricsServer
        },
        points,
        cpu_requests,
        cpu_limits,
        memory_requests,
        memory_limits,
        pod_count: matching.len(),
        io_points,
        io_note: io_note(sampler, io_capable),
        note,
    }
}

fn io_note(sampler: &Arc<ClusterSampler>, io_capable: bool) -> Option<String> {
    if !io_capable {
        return Some("Network and disk are reported per pod and per node only.".into());
    }
    sampler.io_note()
}

/// Pods a target covers.
async fn pods_for_target(
    cluster: &Arc<ClusterHandle>,
    pods: &[Pod],
    target: &MetricTarget,
) -> Vec<Pod> {
    match target {
        MetricTarget::Pod { namespace, name } => pods
            .iter()
            .filter(|pod| pod.namespace().as_deref() == Some(namespace) && pod.name_any() == *name)
            .cloned()
            .collect(),
        MetricTarget::Namespace { name } => pods
            .iter()
            .filter(|pod| pod.namespace().as_deref() == Some(name))
            .cloned()
            .collect(),
        MetricTarget::Node { name } => pods
            .iter()
            .filter(|pod| pod.spec.as_ref().and_then(|s| s.node_name.as_deref()) == Some(name))
            .cloned()
            .collect(),
        MetricTarget::Workload {
            namespace,
            resource,
            name,
        } => {
            let Some(selector) = workload_selector(cluster, resource, namespace, name).await else {
                return Vec::new();
            };
            pods.iter()
                .filter(|pod| {
                    pod.namespace().as_deref() == Some(namespace)
                        && selector.iter().all(|(key, value)| {
                            pod.labels().get(key).map(String::as_str) == Some(value.as_str())
                        })
                })
                .cloned()
                .collect()
        }
    }
}

/// `spec.selector.matchLabels` for a workload, or the flat selector for kinds
/// that use one.
async fn workload_selector(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Option<std::collections::BTreeMap<String, String>> {
    let object = k8s_core::objects::get(cluster, resource, Some(namespace), name)
        .await
        .ok()?;
    let selector = object
        .data
        .get("spec")?
        .get("selector")
        .and_then(|s| s.get("matchLabels").or(Some(s)))?
        .as_object()?;

    Some(
        selector
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect(),
    )
}

/// Sum of declared requests and limits over the matching pods.
fn declared(pods: &[Pod]) -> (f64, f64, f64, f64) {
    let mut cpu_requests = 0.0;
    let mut cpu_limits = 0.0;
    let mut memory_requests = 0.0;
    let mut memory_limits = 0.0;

    for pod in pods {
        if let Some(spec) = &pod.spec {
            let (cpu_request, cpu_limit) = overview::pod_resources_public(spec, "cpu");
            let (memory_request, memory_limit) = overview::pod_resources_public(spec, "memory");
            cpu_requests += cpu_request;
            cpu_limits += cpu_limit;
            memory_requests += memory_request;
            memory_limits += memory_limit;
        }
    }
    (cpu_requests, cpu_limits, memory_requests, memory_limits)
}

async fn prometheus_points(
    cluster: &Arc<ClusterHandle>,
    promql: &prometheus::PrometheusTarget,
    target: &MetricTarget,
    matching: &[Pod],
    window_ms: i64,
) -> Result<Vec<Point>, String> {
    let end = k8s_openapi::jiff::Timestamp::now().as_second();
    let start = end - (window_ms / 1000).max(60);
    // Aim for a few hundred points: enough detail to see a spike, few enough
    // that the query stays cheap and the SVG stays small.
    let step = ((end - start) / 240).max(15);

    let (cpu_query, memory_query) = match target {
        MetricTarget::Pod { namespace, name } => (
            queries::pod_cpu(namespace, name),
            queries::pod_memory(namespace, name),
        ),
        MetricTarget::Namespace { name } => (
            queries::namespace_cpu(name),
            queries::namespace_memory(name),
        ),
        MetricTarget::Node { name } => (queries::node_cpu(name), queries::node_memory(name)),
        MetricTarget::Workload { namespace, .. } => {
            if matching.is_empty() {
                return Ok(Vec::new());
            }
            // A regex alternation over the workload's pods, so a rollout's old
            // and new pods both appear in one series.
            let names = matching
                .iter()
                .map(|pod| pod.name_any())
                .collect::<Vec<_>>()
                .join("|");
            (
                format!(
                    "sum(rate(container_cpu_usage_seconds_total{{namespace=\"{namespace}\",pod=~\"{names}\",container!=\"\",container!=\"POD\"}}[2m]))"
                ),
                format!(
                    "sum(container_memory_working_set_bytes{{namespace=\"{namespace}\",pod=~\"{names}\",container!=\"\",container!=\"POD\"}})"
                ),
            )
        }
    };

    let cpu = prometheus::query_range(cluster, promql, &cpu_query, start, end, step).await?;
    let memory = prometheus::query_range(cluster, promql, &memory_query, start, end, step).await?;

    let cpu_points = cpu.first().map(|s| s.points.clone()).unwrap_or_default();
    let memory_points: std::collections::HashMap<i64, f64> = memory
        .first()
        .map(|s| s.points.iter().copied().collect())
        .unwrap_or_default();

    Ok(cpu_points
        .into_iter()
        .map(|(at, cpu)| Point {
            at,
            cpu,
            memory: memory_points.get(&at).copied().unwrap_or(0.0),
            ..Point::default()
        })
        .collect())
}
