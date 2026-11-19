//! Port forwards, shared by every front end.
//!
//! A forward only reaches into the cluster; it changes nothing there, so a
//! protected context may still have one. Starting one is audited, because it
//! puts an in-cluster service on this machine, and on the network when asked.

use std::{fmt::Display, sync::Arc};

use k8s_ops::forward::{
    ForwardId, ForwardManager, ForwardSpec, ForwardStatus, PortOption, target_ports,
};

use crate::{sources::ClusterSource, state::AppCore};

/// What port forwarding needs of the app beyond a cluster: the manager that
/// holds the running forwards, and the audit log a start is written to.
pub trait ForwardHost: ClusterSource {
    fn forwards(&self) -> &ForwardManager;

    /// Append an action and its outcome to the audit log.
    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    );
}

impl ForwardHost for AppCore {
    fn forwards(&self) -> &ForwardManager {
        &self.forwards
    }

    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        self.record_audit(cluster, action, target, result);
    }
}

impl<H: ForwardHost + ?Sized> ForwardHost for Arc<H> {
    fn forwards(&self) -> &ForwardManager {
        (**self).forwards()
    }

    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        (**self).audit(cluster, action, target, result);
    }
}

/// Start forwarding a local port to a pod, service or workload.
pub async fn start(
    core: &impl ForwardHost,
    cluster: &str,
    spec: ForwardSpec,
) -> Result<ForwardStatus, String> {
    let handle = core.handle(cluster)?;
    let target = format!(
        "{}/{}/{}:{}",
        spec.namespace, spec.resource, spec.name, spec.remote_port
    );
    let action = if spec.expose_on_network {
        "portForwardNetwork"
    } else {
        "portForward"
    };
    let result = core.forwards().start(&handle, spec).await;
    core.audit(cluster, action, &target, &result);
    result.map_err(|e| e.to_string())
}

pub fn stop(core: &impl ForwardHost, id: ForwardId) {
    core.forwards().stop(id);
}

/// The forwards of one cluster, oldest first.
pub fn list(core: &impl ForwardHost, cluster: &str) -> Vec<ForwardStatus> {
    let mut forwards = core.forwards().list();
    forwards.retain(|forward| forward.cluster == cluster);
    forwards
}

/// Ports a target exposes, to suggest in the form.
pub async fn ports(
    core: &impl ForwardHost,
    cluster: &str,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Result<Vec<PortOption>, String> {
    let handle = core.handle(cluster)?;
    target_ports(&handle, resource, namespace, name)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use k8s_core::cluster::ClusterHandle;
    use k8s_ops::forward::{ForwardManager, ForwardSpec};

    use super::{ForwardHost, list, start};
    use crate::sources::ClusterSource;

    /// An app with no clusters that remembers what was audited.
    #[derive(Default)]
    struct NoClusters {
        forwards: ForwardManager,
        audited: Mutex<Vec<String>>,
    }

    impl ClusterSource for NoClusters {
        fn handle(&self, cluster: &str) -> Result<Arc<ClusterHandle>, String> {
            Err(format!("no cluster `{cluster}`"))
        }
    }

    impl ForwardHost for NoClusters {
        fn forwards(&self) -> &ForwardManager {
            &self.forwards
        }

        fn audit<T, E: std::fmt::Display>(
            &self,
            cluster: &str,
            action: &str,
            _: &str,
            _: &Result<T, E>,
        ) {
            if let Ok(mut audited) = self.audited.lock() {
                audited.push(format!("{cluster} {action}"));
            }
        }
    }

    fn spec() -> ForwardSpec {
        serde_json::from_value(serde_json::json!({
            "namespace": "shop", "resource": "pods", "name": "web-1",
            "localPort": 8080, "remotePort": 80,
        }))
        .expect("a spec")
    }

    #[tokio::test]
    async fn a_forward_for_a_cluster_the_app_does_not_have_starts_nothing_and_audits_nothing() {
        let app = NoClusters::default();
        let error = start(&app, "prod", spec())
            .await
            .expect_err("no such cluster");
        assert_eq!(error, "no cluster `prod`");
        assert!(list(&app, "prod").is_empty());
        assert!(app.audited.lock().expect("lock").is_empty());
    }
}
