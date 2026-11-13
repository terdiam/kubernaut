//! Helm operations that change a cluster, shared by every front end.
//!
//! Each one refuses a protected cluster, runs helm against a kubeconfig
//! narrowed to the one context, and leaves an audit entry whatever the outcome.
//! Confirming with the user is the front end's job.

use std::{fmt::Display, sync::Arc};

use k8s_core::ClusterHandle;
use k8s_helm::Helm;

use crate::{audit::AuditEntry, state::AppCore};

impl AppCore {
    /// Append an action and its outcome to the audit log.
    pub fn record_audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        self.audit.record(&AuditEntry {
            timestamp: k8s_openapi::jiff::Timestamp::now().to_string(),
            cluster: cluster.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            outcome: match result {
                Ok(_) => "ok".to_string(),
                Err(err) => err.to_string(),
            },
        });
    }
}

/// What every write needs before helm can run.
struct Prepared {
    handle: Arc<ClusterHandle>,
    helm: Arc<Helm>,
    kubeconfig: String,
}

fn prepare(core: &AppCore, cluster: &str, namespace: &str) -> Result<Prepared, String> {
    core.ensure_writable(cluster)?;
    Ok(Prepared {
        handle: core.clusters.require(cluster).map_err(|e| e.to_string())?,
        helm: core.helm().map_err(|e| e.to_string())?,
        kubeconfig: core
            .clusters
            .minified_kubeconfig(cluster, Some(namespace))
            .map_err(|e| e.to_string())?,
    })
}

/// Roll a release back to an earlier revision. Returns helm's output.
pub async fn rollback(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    release: &str,
    revision: i64,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, namespace)?;
    let result = prepared
        .helm
        .rollback(
            &prepared.handle,
            &prepared.kubeconfig,
            release,
            namespace,
            revision,
        )
        .await;
    core.record_audit(
        cluster,
        "helmRollback",
        &format!("{namespace}/{release}"),
        &result,
    );
    result.map_err(|e| e.to_string())
}

/// Remove a release and the objects it installed. Returns helm's output.
pub async fn uninstall(
    core: &AppCore,
    cluster: &str,
    namespace: &str,
    release: &str,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, namespace)?;
    let result = prepared
        .helm
        .uninstall(
            &prepared.handle,
            &prepared.kubeconfig,
            release,
            namespace,
            false,
        )
        .await;
    core.record_audit(
        cluster,
        "helmUninstall",
        &format!("{namespace}/{release}"),
        &result,
    );
    result.map_err(|e| e.to_string())
}
