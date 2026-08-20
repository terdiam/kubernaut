//! Prometheus as a history source.
//!
//! metrics-server only reports *now* — it keeps no history, so a chart built
//! from it starts empty and fills at the rate the app happens to be open. Where
//! a Prometheus-compatible store exists in the cluster, range queries give real
//! history immediately.
//!
//! Queries go through the apiserver's service proxy rather than a direct
//! connection. That means no port-forward, no second set of credentials, and
//! the user's own RBAC decides whether the read is allowed.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Service;
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A Prometheus-compatible service reachable through the apiserver proxy.
/// Thanos Query, VictoriaMetrics and Mimir all speak the same API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrometheusTarget {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    /// How this target was found, shown in settings so the choice is auditable.
    pub discovered_by: String,
}

impl PrometheusTarget {
    /// Path prefix for the apiserver service proxy.
    fn proxy_base(&self) -> String {
        format!(
            "/api/v1/namespaces/{}/services/{}:{}/proxy",
            self.namespace, self.service, self.port
        )
    }
}

/// Service names that are Prometheus-compatible query endpoints, most specific
/// first. Names, not just labels: several charts label inconsistently but the
/// service name is stable.
const KNOWN_SERVICES: &[&str] = &[
    "prometheus-operated",
    "prometheus-k8s",
    "kube-prometheus-stack-prometheus",
    "prometheus-server",
    "prometheus",
    "thanos-query",
    "thanos-query-frontend",
    "vmselect",
    "victoria-metrics-single-server",
    "mimir-query-frontend",
];

/// Ports these services expose their query API on.
const KNOWN_PORT_NAMES: &[&str] = &["web", "http-web", "http", "query", "grpc-query", "9090"];

/// Find a query endpoint, or `None` when the cluster has none.
///
/// Absence is normal and not an error: metrics-server alone still drives the
/// live gauges.
pub async fn discover(cluster: &Arc<ClusterHandle>) -> Option<PrometheusTarget> {
    let api: Api<Service> = Api::all(cluster.client.clone());
    let services = match api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(err) => {
            tracing::debug!(%err, "cannot list services for Prometheus discovery");
            return None;
        }
    };

    let mut best: Option<(usize, PrometheusTarget)> = None;

    for service in services.iter() {
        let name = service.name_any();
        let namespace = service.namespace().unwrap_or_default();
        let Some(spec) = &service.spec else { continue };

        // Headless services have no cluster IP to proxy to.
        if spec.cluster_ip.as_deref() == Some("None") {
            continue;
        }

        let by_name = KNOWN_SERVICES.iter().position(|known| *known == name);
        let by_label = service
            .labels()
            .get("app.kubernetes.io/name")
            .map(String::as_str)
            .filter(|value| {
                matches!(
                    *value,
                    "prometheus" | "kube-prometheus-stack-prometheus" | "thanos-query" | "vmselect"
                )
            })
            .map(|_| KNOWN_SERVICES.len());

        let (rank, reason) = match (by_name, by_label) {
            (Some(rank), _) => (rank, format!("service name `{name}`")),
            (None, Some(rank)) => (rank, "label app.kubernetes.io/name".to_string()),
            (None, None) => continue,
        };

        let port = spec.ports.iter().flatten().find(|port| {
            port.name
                .as_deref()
                .is_some_and(|n| KNOWN_PORT_NAMES.contains(&n))
                || port.port == 9090
                || port.port == 8481
                || port.port == 8080
        });
        let Some(port) = port.and_then(|p| u16::try_from(p.port).ok()) else {
            continue;
        };

        let candidate = PrometheusTarget {
            namespace,
            service: name,
            port,
            discovered_by: reason,
        };
        if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
            best = Some((rank, candidate));
        }
    }

    let target = best.map(|(_, target)| target)?;

    // Prove it answers before advertising it, so a same-named service that is
    // not Prometheus does not silently produce empty charts.
    match probe(cluster, &target).await {
        Ok(()) => {
            tracing::info!(
                namespace = %target.namespace,
                service = %target.service,
                "Prometheus-compatible endpoint discovered"
            );
            Some(target)
        }
        Err(err) => {
            tracing::debug!(%err, service = %target.service, "candidate did not answer a Prometheus query");
            None
        }
    }
}

async fn probe(cluster: &Arc<ClusterHandle>, target: &PrometheusTarget) -> Result<(), String> {
    let value = get(cluster, target, "/api/v1/query?query=up").await?;
    if value.get("status").and_then(Value::as_str) == Some("success") {
        Ok(())
    } else {
        Err("endpoint did not return a Prometheus success envelope".into())
    }
}

async fn get(
    cluster: &Arc<ClusterHandle>,
    target: &PrometheusTarget,
    path_and_query: &str,
) -> Result<Value, String> {
    let uri = format!("{}{path_and_query}", target.proxy_base());
    let request = http::Request::builder()
        .uri(uri)
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|err| err.to_string())?;

    cluster
        .client
        .request::<Value>(request)
        .await
        .map_err(|err| err.to_string())
}

/// One time series returned by a range query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// Metric labels, for legends and grouping.
    pub labels: std::collections::BTreeMap<String, String>,
    /// `(unix millis, value)` pairs, oldest first.
    pub points: Vec<(i64, f64)>,
}

/// Run a PromQL range query.
///
/// `start`/`end` are unix seconds and `step` is seconds between points.
pub async fn query_range(
    cluster: &Arc<ClusterHandle>,
    target: &PrometheusTarget,
    query: &str,
    start: i64,
    end: i64,
    step: i64,
) -> Result<Vec<Series>, String> {
    let encoded = urlencode(query);
    let path = format!(
        "/api/v1/query_range?query={encoded}&start={start}&end={end}&step={}s",
        step.max(1)
    );
    let body = get(cluster, target, &path).await?;

    if body.get("status").and_then(Value::as_str) != Some("success") {
        return Err(body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("query failed")
            .to_string());
    }

    let mut out = Vec::new();
    for result in body
        .pointer("/data/result")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let labels = result
            .get("metric")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let mut points = Vec::new();
        for point in result
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(pair) = point.as_array() else {
                continue;
            };
            let (Some(at), Some(raw)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let at_seconds = at.as_f64().unwrap_or(0.0);
            // Prometheus encodes sample values as strings so NaN and Inf
            // survive JSON; parsing failures mean a gap, not a zero.
            let Some(value) = raw.as_str().and_then(|s| s.parse::<f64>().ok()) else {
                continue;
            };
            if value.is_finite() {
                points.push(((at_seconds * 1000.0) as i64, value));
            }
        }
        out.push(Series { labels, points });
    }
    Ok(out)
}

/// Percent-encode a PromQL expression for a query string.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// PromQL for the charts the app draws. Kept in one place so the metric names —
/// which differ subtly between kube-state-metrics versions — are easy to audit.
pub mod queries {
    /// CPU cores used by a pod, summed over its containers.
    pub fn pod_cpu(namespace: &str, pod: &str) -> String {
        format!(
            "sum(rate(container_cpu_usage_seconds_total{{namespace=\"{namespace}\",pod=\"{pod}\",container!=\"\",container!=\"POD\"}}[2m]))"
        )
    }

    /// Working-set bytes for a pod — the number the OOM killer looks at, unlike
    /// `container_memory_usage_bytes` which includes reclaimable page cache.
    pub fn pod_memory(namespace: &str, pod: &str) -> String {
        format!(
            "sum(container_memory_working_set_bytes{{namespace=\"{namespace}\",pod=\"{pod}\",container!=\"\",container!=\"POD\"}})"
        )
    }

    pub fn namespace_cpu(namespace: &str) -> String {
        format!(
            "sum(rate(container_cpu_usage_seconds_total{{namespace=\"{namespace}\",container!=\"\",container!=\"POD\"}}[2m]))"
        )
    }

    pub fn namespace_memory(namespace: &str) -> String {
        format!(
            "sum(container_memory_working_set_bytes{{namespace=\"{namespace}\",container!=\"\",container!=\"POD\"}})"
        )
    }

    pub fn node_cpu(node: &str) -> String {
        format!(
            "sum(rate(container_cpu_usage_seconds_total{{node=\"{node}\",container!=\"\",container!=\"POD\"}}[2m]))"
        )
    }

    pub fn node_memory(node: &str) -> String {
        format!(
            "sum(container_memory_working_set_bytes{{node=\"{node}\",container!=\"\",container!=\"POD\"}})"
        )
    }

    pub fn cluster_cpu() -> String {
        "sum(rate(container_cpu_usage_seconds_total{container!=\"\",container!=\"POD\"}[2m]))"
            .to_string()
    }

    pub fn cluster_memory() -> String {
        "sum(container_memory_working_set_bytes{container!=\"\",container!=\"POD\"})".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promql_is_percent_encoded() {
        let encoded = urlencode("sum(rate(x{a=\"b\"}[2m]))");
        assert!(!encoded.contains('{'), "{encoded}");
        assert!(!encoded.contains('"'), "{encoded}");
        assert!(encoded.contains("%7B"), "{encoded}");
    }

    #[test]
    fn proxy_path_targets_the_service_subresource() {
        let target = PrometheusTarget {
            namespace: "monitoring".into(),
            service: "prometheus-operated".into(),
            port: 9090,
            discovered_by: "test".into(),
        };
        assert_eq!(
            target.proxy_base(),
            "/api/v1/namespaces/monitoring/services/prometheus-operated:9090/proxy"
        );
    }

    /// Working-set, not `usage`: the latter counts reclaimable cache and makes
    /// every pod look near its limit.
    #[test]
    fn memory_queries_use_working_set() {
        assert!(queries::pod_memory("ns", "web").contains("working_set"));
        assert!(queries::cluster_memory().contains("working_set"));
    }

    #[test]
    fn pod_queries_exclude_the_pause_container() {
        let query = queries::pod_cpu("ns", "web");
        assert!(query.contains("container!=\\\"POD\\\"") || query.contains("container!=\"POD\""));
    }
}
