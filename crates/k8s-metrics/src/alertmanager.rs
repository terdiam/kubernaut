//! Alertmanager, reached the same way as Prometheus: through the apiserver
//! service proxy, no port-forward, the user's own RBAC deciding the read.
//! `kube-prometheus-stack` and similar bundles deploy it alongside Prometheus,
//! discoverable by the same rank-known-names-then-probe approach.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Service;
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// An Alertmanager reachable through the apiserver proxy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AlertmanagerTarget {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    /// How this target was found, shown in settings so the choice is auditable.
    pub discovered_by: String,
}

impl AlertmanagerTarget {
    fn proxy_base(&self) -> String {
        format!(
            "/api/v1/namespaces/{}/services/{}:{}/proxy",
            self.namespace, self.service, self.port
        )
    }
}

const KNOWN_SERVICES: &[&str] = &[
    "alertmanager-operated",
    "alertmanager-main",
    "kube-prometheus-stack-alertmanager",
    "alertmanager",
];

const KNOWN_PORT_NAMES: &[&str] = &["web", "http-web", "http"];

/// Find an Alertmanager endpoint, or `None` when the cluster has none.
/// Absence is normal and not an error — most of the app works without one.
pub async fn discover(cluster: &Arc<ClusterHandle>) -> Option<AlertmanagerTarget> {
    let api: Api<Service> = Api::all(cluster.client.clone());
    let services = match api.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(err) => {
            tracing::debug!(%err, "cannot list services for Alertmanager discovery");
            return None;
        }
    };

    let mut best: Option<(usize, AlertmanagerTarget)> = None;

    for service in services.iter() {
        let name = service.name_any();
        let namespace = service.namespace().unwrap_or_default();
        let Some(spec) = &service.spec else { continue };

        if spec.cluster_ip.as_deref() == Some("None") {
            continue;
        }

        let by_name = KNOWN_SERVICES.iter().position(|known| *known == name);
        let by_label = service
            .labels()
            .get("app.kubernetes.io/name")
            .map(String::as_str)
            .filter(|value| *value == "alertmanager")
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
                || port.port == 9093
        });
        let Some(port) = port.and_then(|p| u16::try_from(p.port).ok()) else {
            continue;
        };

        let candidate = AlertmanagerTarget {
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

    match probe(cluster, &target).await {
        Ok(()) => {
            tracing::info!(
                namespace = %target.namespace,
                service = %target.service,
                "Alertmanager discovered"
            );
            Some(target)
        }
        Err(err) => {
            tracing::debug!(%err, service = %target.service, "candidate did not answer an Alertmanager status check");
            None
        }
    }
}

async fn probe(cluster: &Arc<ClusterHandle>, target: &AlertmanagerTarget) -> Result<(), String> {
    let value = get(cluster, target, "/api/v2/status").await?;
    if value.get("versionInfo").is_some() || value.get("cluster").is_some() {
        Ok(())
    } else {
        Err("endpoint did not return an Alertmanager status document".into())
    }
}

async fn request(
    cluster: &Arc<ClusterHandle>,
    method: http::Method,
    target: &AlertmanagerTarget,
    path_and_query: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let uri = format!("{}{path_and_query}", target.proxy_base());
    let payload = match &body {
        Some(value) => serde_json::to_vec(value).map_err(|err| err.to_string())?,
        None => Vec::new(),
    };
    let mut builder = http::Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(http::header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .header(http::header::ACCEPT, "application/json")
        .body(payload)
        .map_err(|err| err.to_string())?;

    cluster
        .client
        .request::<Value>(request)
        .await
        .map_err(|err| err.to_string())
}

async fn get(
    cluster: &Arc<ClusterHandle>,
    target: &AlertmanagerTarget,
    path_and_query: &str,
) -> Result<Value, String> {
    request(cluster, http::Method::GET, target, path_and_query, None).await
}

/// One alert as Alertmanager's `GET /api/v2/alerts` reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub fingerprint: String,
    pub labels: std::collections::BTreeMap<String, String>,
    pub annotations: std::collections::BTreeMap<String, String>,
    pub starts_at: String,
    pub ends_at: String,
    /// "active" | "suppressed" | "unprocessed".
    pub state: String,
    pub silenced_by: Vec<String>,
}

pub async fn list_alerts(
    cluster: &Arc<ClusterHandle>,
    target: &AlertmanagerTarget,
) -> Result<Vec<Alert>, String> {
    let body = get(cluster, target, "/api/v2/alerts").await?;
    let array = body
        .as_array()
        .ok_or_else(|| "unexpected response shape from /api/v2/alerts".to_string())?;

    Ok(array
        .iter()
        .map(|entry| Alert {
            fingerprint: entry
                .get("fingerprint")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            labels: string_map(entry.get("labels")),
            annotations: string_map(entry.get("annotations")),
            starts_at: entry
                .get("startsAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            ends_at: entry
                .get("endsAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            state: entry
                .pointer("/status/state")
                .and_then(Value::as_str)
                .unwrap_or("active")
                .to_string(),
            silenced_by: entry
                .pointer("/status/silencedBy")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        })
        .collect())
}

fn string_map(value: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// A new silence, matching Alertmanager's `POST /api/v2/silences` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceRequest {
    pub matcher_name: String,
    pub matcher_value: String,
    /// RFC3339. Alertmanager rejects a silence that has already ended.
    pub ends_at: String,
    pub created_by: String,
    pub comment: String,
}

pub async fn post_silence(
    cluster: &Arc<ClusterHandle>,
    target: &AlertmanagerTarget,
    silence: &SilenceRequest,
) -> Result<String, String> {
    let now = k8s_openapi::jiff::Timestamp::now().to_string();
    let body = json!({
        "matchers": [{
            "name": silence.matcher_name,
            "value": silence.matcher_value,
            "isRegex": false,
            "isEqual": true,
        }],
        "startsAt": now,
        "endsAt": silence.ends_at,
        "createdBy": silence.created_by,
        "comment": silence.comment,
    });
    let response = request(
        cluster,
        http::Method::POST,
        target,
        "/api/v2/silences",
        Some(body),
    )
    .await?;
    response
        .get("silenceID")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Alertmanager did not return a silenceID".to_string())
}

pub async fn delete_silence(
    cluster: &Arc<ClusterHandle>,
    target: &AlertmanagerTarget,
    id: &str,
) -> Result<(), String> {
    request(
        cluster,
        http::Method::DELETE,
        target,
        &format!("/api/v2/silence/{id}"),
        None,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_path_targets_the_service_subresource() {
        let target = AlertmanagerTarget {
            namespace: "monitoring".into(),
            service: "alertmanager-operated".into(),
            port: 9093,
            discovered_by: "test".into(),
        };
        assert_eq!(
            target.proxy_base(),
            "/api/v1/namespaces/monitoring/services/alertmanager-operated:9093/proxy"
        );
    }
}
