//! Overview and history for the cluster dashboard.

use k8s_metrics::recommend::{self, Recommendation};
use k8s_metrics::{
    ClusterOverview, MetricTarget, NamespaceUsage, NodeScope, NodeSummary, ObjectMetrics,
    PrometheusTarget, Sample, Topology,
};
use serde::Serialize;
use tauri::State;

use crate::{error::CommandResult, state::AppState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPayload {
    /// `None` while the first sample is still being collected.
    pub overview: Option<ClusterOverview>,
    /// True once node state has loaded, so "empty" can be told from "loading".
    pub ready: bool,
}

#[tauri::command]
pub async fn cluster_overview(
    state: State<'_, AppState>,
    cluster: String,
    scope: NodeScope,
) -> CommandResult<OverviewPayload> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    Ok(OverviewPayload {
        overview: sampler.overview(scope),
        ready: sampler.ready(),
    })
}

/// `window_ms` is how much history to return, counted back from the newest
/// sample rather than from wall-clock now — after the app has been idle the two
/// differ, and anchoring to now would return an empty chart.
#[tauri::command]
pub async fn overview_history(
    state: State<'_, AppState>,
    cluster: String,
    scope: NodeScope,
    window_ms: i64,
) -> CommandResult<Vec<Sample>> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);

    let all = sampler.history(scope, 0);
    let Some(newest) = all.last().map(|s| s.at) else {
        return Ok(Vec::new());
    };
    Ok(all
        .into_iter()
        .filter(|s| s.at >= newest - window_ms)
        .collect())
}

/// Heatmap rows: usage against declared requests and limits, per namespace.
#[tauri::command]
pub async fn namespace_usage(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<Vec<NamespaceUsage>> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    // Pod-level metrics are only polled while something asks for them; asking
    // here is what keeps the heatmap fed.
    sampler.request_pod_metrics();
    Ok(sampler.namespace_usage())
}

/// Charts for one pod, node, namespace or workload.
#[tauri::command]
pub async fn object_metrics(
    state: State<'_, AppState>,
    cluster: String,
    target: MetricTarget,
    window_ms: i64,
) -> CommandResult<ObjectMetrics> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    Ok(k8s_metrics::resolve::fetch(&handle, &sampler, &target, window_ms).await)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSources {
    /// Present when a Prometheus-compatible endpoint was found.
    pub prometheus: Option<PrometheusTarget>,
    /// False until discovery has finished.
    pub checked: bool,
}

#[tauri::command]
pub async fn metrics_sources(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<MetricsSources> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    Ok(MetricsSources {
        prometheus: sampler.prometheus(),
        checked: sampler.prometheus_checked(),
    })
}

/// Ingress → Service → Workload → Pod → Node graph for some namespaces.
#[tauri::command]
pub async fn topology(
    state: State<'_, AppState>,
    cluster: String,
    namespaces: Vec<String>,
) -> CommandResult<Topology> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    let pods = sampler.pods();
    k8s_metrics::topology::build(&handle, &namespaces, &pods)
        .await
        .map_err(crate::error::CommandError::new)
}

/// Live CPU, memory, pod and system figures per node, for the node list.
#[tauri::command]
pub async fn node_summaries(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<Vec<NodeSummary>> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);
    Ok(sampler.node_summaries())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizingReport {
    pub workload: String,
    pub namespace: String,
    /// Pods currently matched. Recommendations treat each as an independent
    /// observation of the same container.
    pub pods: usize,
    pub recommendations: Vec<Recommendation>,
    /// Set when nothing could be measured, explaining why.
    pub note: Option<String>,
}

/// Request and limit suggestions for a workload, from observed usage.
///
/// Observation is demand-driven: the first call starts sampling and returns
/// almost nothing, and the numbers improve while the panel stays open. That is
/// stated in the result rather than hidden behind a spinner.
#[tauri::command]
pub async fn workload_sizing(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    resource: String,
    name: String,
) -> CommandResult<SizingReport> {
    let handle = state.clusters.require(&cluster)?;
    let sampler = state.metrics.ensure(&handle);

    let object = k8s_core::objects::get(&handle, &resource, Some(&namespace), &name).await?;
    let value = k8s_core::objects::to_value(&object)?;

    let selector: std::collections::BTreeMap<String, String> = value
        .pointer("/spec/selector/matchLabels")
        .and_then(|labels| labels.as_object())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let key = format!("{namespace}/{resource}/{name}");
    if selector.is_empty() {
        return Ok(SizingReport {
            workload: name,
            namespace,
            pods: 0,
            recommendations: Vec::new(),
            note: Some(
                "This object has no pod selector, so its containers cannot be matched to usage."
                    .into(),
            ),
        });
    }
    sampler.observe_workload(&key, &namespace, selector);

    let containers = value
        .pointer("/spec/template/spec/containers")
        .and_then(|containers| containers.as_array())
        .cloned()
        .unwrap_or_default();

    let number = |container: &serde_json::Value, bucket: &str, key: &str| -> f64 {
        container
            .pointer(&format!("/resources/{bucket}/{key}"))
            .and_then(|value| value.as_str())
            .and_then(k8s_metrics::quantity::parse)
            .unwrap_or(0.0)
    };

    let mut recommendations = Vec::new();
    for container in &containers {
        let Some(container_name) = container.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        let samples = sampler.container_history(&key, container_name);
        let window = match (samples.first(), samples.last()) {
            (Some((first, _, _)), Some((last, _, _))) => (last - first) / 1000,
            _ => 0,
        };
        let pairs: Vec<(f64, f64)> = samples
            .iter()
            .map(|(_, cpu, memory)| (*cpu, *memory))
            .collect();

        recommendations.push(recommend::build(
            container_name,
            &pairs,
            window,
            number(container, "requests", "cpu"),
            number(container, "limits", "cpu"),
            number(container, "requests", "memory"),
            number(container, "limits", "memory"),
        ));
    }

    // Pods matched by the selector, which is what the recommendation averaged
    // over — not every pod in the namespace.
    let pods = sampler
        .pods()
        .iter()
        .filter(|pod| pod.metadata.namespace.as_deref() == Some(namespace.as_str()))
        .count();

    Ok(SizingReport {
        workload: name,
        namespace,
        pods,
        recommendations,
        note: None,
    })
}
