//! Network and filesystem statistics from the kubelet.
//!
//! `metrics.k8s.io` reports CPU and memory only. Network counters and disk
//! usage come from each kubelet's own summary endpoint, reached through the
//! apiserver's node proxy — the same path `kubectl top` and cAdvisor scrapers
//! use, so no extra component has to be installed.
//!
//! Requires the `nodes/proxy` RBAC verb. Plenty of read-only users do not have
//! it, so a refusal is reported rather than treated as an error.

use std::{collections::HashMap, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use serde_json::Value;

/// Cumulative counters for one pod, as the kubelet reports them.
///
/// Network figures are monotonic totals since the pod started, not rates —
/// charting them raw draws an ever-rising line that says nothing. The sampler
/// differentiates them between ticks.
#[derive(Debug, Clone, Copy, Default)]
pub struct PodStats {
    pub rx_bytes: f64,
    pub tx_bytes: f64,
    /// Container writable layers plus logs.
    pub fs_used_bytes: f64,
    pub fs_capacity_bytes: f64,
    /// Mounted volumes, summed.
    pub volume_used_bytes: f64,
    pub volume_capacity_bytes: f64,
    pub ephemeral_used_bytes: f64,
}

/// Disk figures for the node itself, as the kubelet reports them.
///
/// `fs` is the filesystem holding the kubelet's working directory — the one
/// that fills up and triggers eviction. `image_fs` is where the container
/// runtime stores images; on most installs it is the same device, but when it
/// is separate it is usually the one that actually runs out.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeFilesystem {
    pub used_bytes: f64,
    pub capacity_bytes: f64,
    pub image_used_bytes: f64,
    pub image_capacity_bytes: f64,
}

/// Fetch node-level disk usage.
pub async fn node_filesystem(
    cluster: &Arc<ClusterHandle>,
    node: &str,
) -> Result<NodeFilesystem, String> {
    let body = summary_body(cluster, node).await?;
    let number = |path: &str| body.pointer(path).and_then(Value::as_f64).unwrap_or(0.0);

    Ok(NodeFilesystem {
        used_bytes: number("/node/fs/usedBytes"),
        capacity_bytes: number("/node/fs/capacityBytes"),
        image_used_bytes: number("/node/runtime/imageFs/usedBytes"),
        image_capacity_bytes: number("/node/runtime/imageFs/capacityBytes"),
    })
}

async fn summary_body(cluster: &Arc<ClusterHandle>, node: &str) -> Result<Value, String> {
    let request = http::Request::builder()
        .uri(format!("/api/v1/nodes/{node}/proxy/stats/summary"))
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|err| err.to_string())?;

    cluster
        .client
        .request::<Value>(request)
        .await
        .map_err(|err| match err {
            kube::Error::Api(status) if status.code == 403 => {
                "no permission for nodes/proxy, so network and disk stats are unavailable"
                    .to_string()
            }
            kube::Error::Api(status) if status.code == 404 => {
                "this kubelet does not expose /stats/summary".to_string()
            }
            other => other.to_string(),
        })
}

/// Fetch the summary for one node. Keyed `namespace/pod`.
pub async fn node_summary(
    cluster: &Arc<ClusterHandle>,
    node: &str,
) -> Result<HashMap<String, PodStats>, String> {
    let body = summary_body(cluster, node).await?;
    let mut out = HashMap::new();
    for pod in body
        .get("pods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(namespace), Some(name)) = (
            pod.pointer("/podRef/namespace").and_then(Value::as_str),
            pod.pointer("/podRef/name").and_then(Value::as_str),
        ) else {
            continue;
        };

        let number = |value: Option<&Value>| value.and_then(Value::as_f64).unwrap_or(0.0);

        let mut stats = PodStats {
            rx_bytes: number(pod.pointer("/network/rxBytes")),
            tx_bytes: number(pod.pointer("/network/txBytes")),
            ephemeral_used_bytes: number(pod.pointer("/ephemeral-storage/usedBytes")),
            ..PodStats::default()
        };

        // Writable layer and logs, summed over the pod's containers.
        for container in pod
            .get("containers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            stats.fs_used_bytes += number(container.pointer("/rootfs/usedBytes"));
            stats.fs_used_bytes += number(container.pointer("/logs/usedBytes"));
            stats.fs_capacity_bytes = stats
                .fs_capacity_bytes
                .max(number(container.pointer("/rootfs/capacityBytes")));
        }

        for volume in pod
            .get("volume")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            stats.volume_used_bytes += number(volume.get("usedBytes"));
            stats.volume_capacity_bytes += number(volume.get("capacityBytes"));
        }

        out.insert(format!("{namespace}/{name}"), stats);
    }

    Ok(out)
}

/// Turn two cumulative samples into a per-second rate.
///
/// A counter reset (pod restart, interface recreated) shows up as a decrease;
/// reporting the negative difference as a huge negative rate would be worse
/// than reporting nothing, so a reset yields zero.
pub fn rate(previous: f64, current: f64, seconds: f64) -> f64 {
    if seconds <= 0.0 || current < previous {
        return 0.0;
    }
    (current - previous) / seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_bytes_per_second() {
        assert_eq!(rate(1000.0, 2500.0, 15.0), 100.0);
    }

    /// Counters reset when a pod restarts; the chart must not spike downward.
    #[test]
    fn counter_reset_reads_as_zero_not_negative() {
        assert_eq!(rate(9_000_000.0, 12.0, 15.0), 0.0);
    }

    #[test]
    fn zero_interval_is_not_a_division_by_zero() {
        assert_eq!(rate(0.0, 100.0, 0.0), 0.0);
    }
}
