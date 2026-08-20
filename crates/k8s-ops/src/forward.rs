//! Port forwarding.
//!
//! The listener always binds loopback unless the user explicitly opts into a
//! wider address: a forward exposes an in-cluster service on the workstation,
//! and binding `0.0.0.0` by default would publish it to the local network.
//!
//! The target pod is resolved per accepted connection rather than once at
//! start. A rollout replaces pods underneath a long-lived forward, and
//! resolving late means the next connection lands on a live pod instead of the
//! forward silently going dead.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use dashmap::DashMap;
use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{Api, ResourceExt, api::ListParams};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::error::{OpsError, Result};

pub type ForwardId = u64;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardSpec {
    pub namespace: String,
    /// `group/version/plural` of the target (pod, service or workload).
    pub resource: String,
    pub name: String,
    /// Port on the pod (or the service port, which is resolved to a targetPort).
    pub remote_port: u16,
    /// `None` or 0 picks a free ephemeral port.
    pub local_port: Option<u16>,
    /// Bind a non-loopback address. Off by default; the UI warns when set.
    #[serde(default)]
    pub expose_on_network: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStatus {
    pub id: ForwardId,
    pub cluster: String,
    pub namespace: String,
    pub resource: String,
    pub name: String,
    pub local_address: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub active_connections: usize,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Last error, if the most recent connection failed.
    pub last_error: Option<String>,
}

struct Forward {
    id: ForwardId,
    cluster: String,
    spec: ForwardSpec,
    local_addr: SocketAddr,
    active: Arc<AtomicUsize>,
    sent: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
    last_error: Arc<parking_lot::Mutex<Option<String>>>,
    cancel: CancellationToken,
}

impl Forward {
    fn status(&self) -> ForwardStatus {
        ForwardStatus {
            id: self.id,
            cluster: self.cluster.clone(),
            namespace: self.spec.namespace.clone(),
            resource: self.spec.resource.clone(),
            name: self.spec.name.clone(),
            local_address: self.local_addr.ip().to_string(),
            local_port: self.local_addr.port(),
            remote_port: self.spec.remote_port,
            active_connections: self.active.load(Ordering::Relaxed),
            bytes_sent: self.sent.load(Ordering::Relaxed),
            bytes_received: self.received.load(Ordering::Relaxed),
            last_error: self.last_error.lock().clone(),
        }
    }
}

#[derive(Default)]
pub struct ForwardManager {
    forwards: Arc<DashMap<ForwardId, Arc<Forward>>>,
    next_id: AtomicU64,
}

impl ForwardManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(
        &self,
        cluster: &Arc<ClusterHandle>,
        spec: ForwardSpec,
    ) -> Result<ForwardStatus> {
        // Fail before binding if the target cannot be resolved, so the user
        // gets "no ready pod" rather than a listener that refuses every
        // connection.
        resolve_target(cluster, &spec).await?;

        let bind_ip = if spec.expose_on_network {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let listener = TcpListener::bind(SocketAddr::new(bind_ip, spec.local_port.unwrap_or(0)))
            .await
            .map_err(|err| {
                OpsError::other(format!(
                    "cannot bind {bind_ip}:{}: {err}",
                    spec.local_port.unwrap_or(0)
                ))
            })?;
        let local_addr = listener.local_addr()?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let forward = Arc::new(Forward {
            id,
            cluster: cluster.id.clone(),
            spec,
            local_addr,
            active: Arc::new(AtomicUsize::new(0)),
            sent: Arc::new(AtomicU64::new(0)),
            received: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(parking_lot::Mutex::new(None)),
            cancel: cluster.cancel_token().child_token(),
        });

        let status = forward.status();
        self.forwards.insert(id, forward.clone());
        spawn_acceptor(cluster.clone(), forward, listener, self.forwards.clone());
        Ok(status)
    }

    pub fn stop(&self, id: ForwardId) {
        if let Some((_, forward)) = self.forwards.remove(&id) {
            forward.cancel.cancel();
        }
    }

    pub fn stop_cluster(&self, cluster: &str) {
        let ids: Vec<ForwardId> = self
            .forwards
            .iter()
            .filter(|f| f.cluster == cluster)
            .map(|f| f.id)
            .collect();
        for id in ids {
            self.stop(id);
        }
    }

    pub fn list(&self) -> Vec<ForwardStatus> {
        let mut out: Vec<ForwardStatus> = self.forwards.iter().map(|f| f.status()).collect();
        out.sort_by_key(|f| f.id);
        out
    }
}

/// The pod and container port a forward should reach.
struct Target {
    pod: String,
    port: u16,
}

async fn resolve_target(cluster: &Arc<ClusterHandle>, spec: &ForwardSpec) -> Result<Target> {
    let pods: Api<Pod> = Api::namespaced(cluster.client.clone(), &spec.namespace);

    // Pods are their own target.
    if spec.resource.ends_with("/pods") {
        return Ok(Target {
            pod: spec.name.clone(),
            port: spec.remote_port,
        });
    }

    if spec.resource.ends_with("/services") {
        let services: Api<Service> = Api::namespaced(cluster.client.clone(), &spec.namespace);
        let service = services.get(&spec.name).await?;
        let service_spec = service
            .spec
            .ok_or_else(|| OpsError::other("service has no spec"))?;

        let selector =
            service_spec
                .selector
                .filter(|s| !s.is_empty())
                .ok_or(OpsError::NoSelector {
                    kind: "Service".into(),
                })?;

        // A service port and the container port behind it are often different;
        // forwarding to the service port number would hit nothing.
        let port = service_spec
            .ports
            .iter()
            .flatten()
            .find(|p| p.port == i32::from(spec.remote_port))
            .and_then(|p| match &p.target_port {
                Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(n)) => {
                    u16::try_from(*n).ok()
                }
                // Named target ports would need the pod spec to resolve; fall
                // back to the service port, which is right for the common case
                // where the name maps to the same number.
                _ => None,
            })
            .unwrap_or(spec.remote_port);

        let label = selector
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        let pod = first_ready_pod(&pods, &label).await?;
        return Ok(Target { pod, port });
    }

    // Workloads: resolve through their pod selector.
    let obj =
        k8s_core::objects::get(cluster, &spec.resource, Some(&spec.namespace), &spec.name).await?;
    let labels = obj
        .data
        .get("spec")
        .and_then(|s| s.get("selector"))
        .and_then(|s| s.get("matchLabels").or(Some(s)))
        .and_then(|s| s.as_object())
        .ok_or_else(|| OpsError::NoSelector {
            kind: spec.resource.clone(),
        })?;
    let label = labels
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(",");
    let pod = first_ready_pod(&pods, &label).await?;
    Ok(Target {
        pod,
        port: spec.remote_port,
    })
}

async fn first_ready_pod(api: &Api<Pod>, label: &str) -> Result<String> {
    let list = api.list(&ListParams::default().labels(label)).await?;
    list.iter()
        .find(|pod| {
            pod.status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|conds| {
                    conds
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                })
        })
        .map(|pod| pod.name_any())
        .ok_or_else(|| OpsError::other(format!("no ready pod matches `{label}`")))
}

fn spawn_acceptor(
    cluster: Arc<ClusterHandle>,
    forward: Arc<Forward>,
    listener: TcpListener,
    registry: Arc<DashMap<ForwardId, Arc<Forward>>>,
) {
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = forward.cancel.cancelled() => break,
                accepted = listener.accept() => accepted,
            };

            let (socket, peer) = match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!(forward = forward.id, %err, "accept failed");
                    *forward.last_error.lock() = Some(err.to_string());
                    continue;
                }
            };

            let cluster = cluster.clone();
            let forward = forward.clone();
            tokio::spawn(async move {
                forward.active.fetch_add(1, Ordering::Relaxed);
                if let Err(err) = pump(&cluster, &forward, socket).await {
                    tracing::debug!(forward = forward.id, %peer, %err, "forwarded connection ended");
                    *forward.last_error.lock() = Some(err.to_string());
                }
                forward.active.fetch_sub(1, Ordering::Relaxed);
            });
        }
        registry.remove(&forward.id);
    });
}

async fn pump(
    cluster: &Arc<ClusterHandle>,
    forward: &Arc<Forward>,
    mut socket: tokio::net::TcpStream,
) -> Result<()> {
    let target = resolve_target(cluster, &forward.spec).await?;
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &forward.spec.namespace);

    let mut pf = api.portforward(&target.pod, &[target.port]).await?;
    let mut upstream = pf
        .take_stream(target.port)
        .ok_or(OpsError::UnknownPort(target.port))?;

    let (sent, received) = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
    forward.sent.fetch_add(sent, Ordering::Relaxed);
    forward.received.fetch_add(received, Ordering::Relaxed);

    // Surface the stream-level error the apiserver reports (for example the
    // container refusing the port), which otherwise vanishes silently.
    if let Some(error) = pf.take_error(target.port)
        && let Some(message) = error.await
    {
        *forward.last_error.lock() = Some(message);
    }
    Ok(())
}

/// Ports a target exposes, used to prefill the forward dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOption {
    pub port: u16,
    pub name: Option<String>,
    pub protocol: String,
}

pub async fn target_ports(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Result<Vec<PortOption>> {
    let obj = k8s_core::objects::get(cluster, resource, Some(namespace), name).await?;
    let value = obj.data;

    let mut out = Vec::new();

    // Service ports.
    for port in value
        .get("spec")
        .and_then(|s| s.get("ports"))
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(number) = port
            .get("port")
            .or_else(|| port.get("containerPort"))
            .and_then(|p| p.as_i64())
            .and_then(|p| u16::try_from(p).ok())
        {
            out.push(PortOption {
                port: number,
                name: port.get("name").and_then(|n| n.as_str()).map(String::from),
                protocol: port
                    .get("protocol")
                    .and_then(|p| p.as_str())
                    .unwrap_or("TCP")
                    .to_string(),
            });
        }
    }

    // Container ports, on pods and on pod templates alike.
    let containers = value
        .pointer("/spec/containers")
        .or_else(|| value.pointer("/spec/template/spec/containers"))
        .and_then(|c| c.as_array());
    for container in containers.into_iter().flatten() {
        for port in container
            .get("ports")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(number) = port
                .get("containerPort")
                .and_then(|p| p.as_i64())
                .and_then(|p| u16::try_from(p).ok())
            {
                out.push(PortOption {
                    port: number,
                    name: port.get("name").and_then(|n| n.as_str()).map(String::from),
                    protocol: port
                        .get("protocol")
                        .and_then(|p| p.as_str())
                        .unwrap_or("TCP")
                        .to_string(),
                });
            }
        }
    }

    out.sort_by_key(|p| p.port);
    out.dedup_by_key(|p| p.port);
    Ok(out)
}
