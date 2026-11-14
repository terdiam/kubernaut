//! Port forwards, shared by every front end.
//!
//! A forward only reaches into the cluster; it changes nothing there, so a
//! protected context may still have one. Starting one is audited, because it
//! puts an in-cluster service on this machine, and on the network when asked.

use k8s_ops::forward::{ForwardId, ForwardSpec, ForwardStatus, PortOption, target_ports};

use crate::state::AppCore;

/// Start forwarding a local port to a pod, service or workload.
pub async fn start(
    core: &AppCore,
    cluster: &str,
    spec: ForwardSpec,
) -> Result<ForwardStatus, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let target = format!(
        "{}/{}/{}:{}",
        spec.namespace, spec.resource, spec.name, spec.remote_port
    );
    let action = if spec.expose_on_network {
        "portForwardNetwork"
    } else {
        "portForward"
    };
    let result = core.forwards.start(&handle, spec).await;
    core.record_audit(cluster, action, &target, &result);
    result.map_err(|e| e.to_string())
}

pub fn stop(core: &AppCore, id: ForwardId) {
    core.forwards.stop(id);
}

/// The forwards of one cluster, oldest first.
pub fn list(core: &AppCore, cluster: &str) -> Vec<ForwardStatus> {
    let mut forwards = core.forwards.list();
    forwards.retain(|forward| forward.cluster == cluster);
    forwards
}

/// Ports a target exposes, to suggest in the form.
pub async fn ports(
    core: &AppCore,
    cluster: &str,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Result<Vec<PortOption>, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    target_ports(&handle, resource, namespace, name)
        .await
        .map_err(|e| e.to_string())
}
