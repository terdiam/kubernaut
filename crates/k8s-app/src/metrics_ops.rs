//! Metrics for one object, shared by every front end: charts, sizing
//! suggestions for a workload, and a namespace's quota.
//!
//! Everything here reads. Sampling is demand-driven: asking about a pod or a
//! workload is what starts watching it, so the first answer is thin and
//! improves while someone keeps asking.

use std::collections::BTreeMap;

use k8s_metrics::{
    MetricTarget, NamespaceQuotaInfo, NamespaceUsage, NodeSummary, ObjectMetrics, Point,
    Recommendation, Topology, quantity, quota, recommend, resolve, topology,
};
use serde::Serialize;
use serde_json::Value;

use crate::state::AppCore;

/// History and current figures for a pod, node, namespace or workload.
pub async fn object_metrics(
    core: &AppCore,
    cluster: &str,
    target: &MetricTarget,
    window_ms: i64,
) -> Result<ObjectMetrics, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let sampler = core.metrics.ensure(&handle);
    Ok(resolve::fetch(&handle, &sampler, target, window_ms).await)
}

/// What a workload's pods use together, right now.
///
/// The sampler keeps a history for a pod, a node and a namespace, but not for a
/// workload, which needs its selector to know whose pods to add up. So the
/// caller keeps the history, one point per call, and this gives it the point.
/// `None` while no matching pod has reported usage.
pub async fn workload_point(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    resource: &str,
    name: &str,
) -> Result<Option<Point>, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let sampler = core.metrics.ensure(&handle);
    // Pod usage is only polled while something asks for it.
    sampler.request_pod_metrics();

    let object = k8s_core::objects::get(&handle, resource, Some(namespace), name)
        .await
        .map_err(|e| e.to_string())?;
    let selector = selector_of(&k8s_core::objects::to_value(&object).map_err(|e| e.to_string())?);
    if selector.is_empty() {
        return Ok(None);
    }
    let names: std::collections::HashSet<String> = sampler
        .pods()
        .iter()
        .filter(|pod| {
            pod.metadata.namespace.as_deref() == Some(namespace)
                && selector.iter().all(|(key, value)| {
                    pod.metadata
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get(key))
                        == Some(value)
                })
        })
        .filter_map(|pod| pod.metadata.name.clone())
        .collect();

    Ok(sum_usage(
        sampler
            .pod_usages(Some(namespace))
            .iter()
            .filter(|usage| names.contains(&usage.name))
            .map(|usage| (usage.usage_available, usage.cpu_usage, usage.memory_usage)),
    ))
}

/// A point adding up the usage that has been measured, or `None` when none has.
fn sum_usage(usages: impl Iterator<Item = (bool, f64, f64)>) -> Option<Point> {
    let (mut cpu, mut memory, mut measured) = (0.0, 0.0, false);
    for (available, pod_cpu, pod_memory) in usages {
        if available {
            cpu += pod_cpu;
            memory += pod_memory;
            measured = true;
        }
    }
    measured.then(|| Point {
        at: k8s_openapi::jiff::Timestamp::now().as_millisecond(),
        cpu,
        memory,
        ..Default::default()
    })
}

/// How traffic and ownership connect in some namespaces: ingresses to services
/// to workloads to pods to nodes. Built per namespace, because a graph of the
/// whole cluster is a hairball nobody can read.
pub async fn topology(
    core: &AppCore,
    cluster: &str,
    namespaces: &[String],
) -> Result<Topology, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let pods = core.metrics.ensure(&handle).pods();
    topology::build(&handle, namespaces, &pods).await
}

/// Live CPU, memory, pod and disk figures per node, for the node list.
///
/// Asking is what makes the sampler collect disk, so the first answer can lack
/// it until the next sample.
pub fn node_summaries(core: &AppCore, cluster: &str) -> Result<Vec<NodeSummary>, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    Ok(core.metrics.ensure(&handle).node_summaries())
}

/// What every namespace uses against what it declares, for the heatmap.
///
/// Pod-level metrics are only polled while someone asks, so asking is what
/// keeps this fed; the first answer can be empty until a sample has completed.
pub fn namespace_usage(core: &AppCore, cluster: &str) -> Result<Vec<NamespaceUsage>, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let sampler = core.metrics.ensure(&handle);
    sampler.request_pod_metrics();
    Ok(sampler.namespace_usage())
}

/// A namespace's `ResourceQuota` and `LimitRange` objects.
pub async fn namespace_quota(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
) -> Result<NamespaceQuotaInfo, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    quota::namespace_quota(&handle, namespace).await
}

/// Request and limit suggestions for a workload, from what its containers used.
#[derive(Debug, Clone, Serialize)]
pub struct SizingReport {
    /// Pods in the namespace the sampler knows of.
    pub pods: usize,
    pub recommendations: Vec<Recommendation>,
    /// Why there is nothing to show, when that is so.
    pub note: Option<String>,
}

/// The labels a workload selects its pods by.
fn selector_of(object: &Value) -> BTreeMap<String, String> {
    object
        .pointer("/spec/selector/matchLabels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// A container's declared quantity, in cores or bytes; zero when unset.
fn declared(container: &Value, bucket: &str, key: &str) -> f64 {
    container
        .pointer(&format!("/resources/{bucket}/{key}"))
        .and_then(Value::as_str)
        .and_then(quantity::parse)
        .unwrap_or(0.0)
}

pub async fn workload_sizing(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    resource: &str,
    name: &str,
) -> Result<SizingReport, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let sampler = core.metrics.ensure(&handle);

    let object = k8s_core::objects::get(&handle, resource, Some(namespace), name)
        .await
        .map_err(|e| e.to_string())?;
    let value = k8s_core::objects::to_value(&object).map_err(|e| e.to_string())?;

    let selector = selector_of(&value);
    if selector.is_empty() {
        return Ok(SizingReport {
            pods: 0,
            recommendations: Vec::new(),
            note: Some(
                "This object has no pod selector, so its containers cannot be matched to usage."
                    .into(),
            ),
        });
    }
    let key = format!("{namespace}/{resource}/{name}");
    sampler.observe_workload(&key, namespace, selector);

    let containers = value
        .pointer("/spec/template/spec/containers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut recommendations = Vec::with_capacity(containers.len());
    for container in &containers {
        let Some(container_name) = container.get("name").and_then(Value::as_str) else {
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
            declared(container, "requests", "cpu"),
            declared(container, "limits", "cpu"),
            declared(container, "requests", "memory"),
            declared(container, "limits", "memory"),
        ));
    }

    let pods = sampler
        .pods()
        .iter()
        .filter(|pod| pod.metadata.namespace.as_deref() == Some(namespace))
        .count();
    Ok(SizingReport {
        pods,
        recommendations,
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{declared, selector_of, sum_usage};

    #[test]
    fn a_workload_selects_its_pods_by_match_labels() {
        let object = json!({"spec": {"selector": {"matchLabels": {"app": "web", "tier": "fe"}}}});
        let selector = selector_of(&object);
        assert_eq!(selector.get("app").map(String::as_str), Some("web"));
        assert_eq!(selector.len(), 2);
        assert!(selector_of(&json!({"spec": {}})).is_empty());
    }

    #[test]
    fn a_declared_quantity_is_read_in_cores_or_bytes_and_zero_when_unset() {
        let container = json!({"resources": {"requests": {"cpu": "250m", "memory": "64Mi"}}});
        assert!((declared(&container, "requests", "cpu") - 0.25).abs() < 1e-9);
        assert_eq!(
            declared(&container, "requests", "memory"),
            64.0 * 1024.0 * 1024.0
        );
        assert_eq!(declared(&container, "limits", "cpu"), 0.0);
    }

    #[test]
    fn a_workloads_point_adds_up_only_the_pods_that_reported() {
        let point =
            sum_usage([(true, 0.25, 100.0), (true, 0.5, 50.0), (false, 9.0, 9.0)].into_iter());
        let point = point.map(|p| (p.cpu, p.memory));
        assert_eq!(point, Some((0.75, 150.0)));
        assert!(sum_usage([(false, 1.0, 1.0)].into_iter()).is_none());
        assert!(sum_usage(std::iter::empty()).is_none());
    }
}
