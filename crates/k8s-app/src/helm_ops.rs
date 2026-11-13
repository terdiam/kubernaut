//! Helm operations that change a cluster, shared by every front end.
//!
//! Each one refuses a protected cluster, runs helm against a kubeconfig
//! narrowed to the one context, and leaves an audit entry whatever the outcome.
//! Confirming with the user is the front end's job.

use std::{fmt::Display, sync::Arc};

use k8s_core::ClusterHandle;
use k8s_helm::{
    Helm, UpgradeDiff, UpgradeOptions,
    cli::{self, ChartRef, ReleaseTarget},
    store,
};

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

/// What an upgrade needs: which release, which chart at which version, and the
/// values to install it with.
#[derive(Debug, Clone)]
pub struct UpgradeRequest {
    pub namespace: String,
    pub release: String,
    /// `repo/chart`, a local path, or an `oci://` reference.
    pub chart: String,
    pub version: Option<String>,
    pub values: String,
    pub options: UpgradeOptions,
}

impl UpgradeRequest {
    fn target(&self) -> ReleaseTarget<'_> {
        ReleaseTarget {
            release: &self.release,
            namespace: &self.namespace,
        }
    }

    fn chart_ref(&self) -> ChartRef<'_> {
        ChartRef {
            reference: &self.chart,
            version: self.version.as_deref(),
        }
    }
}

/// What an upgrade would change, without changing anything.
///
/// Renders the proposed chart and compares it with the manifest helm recorded
/// for the installed revision, which is the comparison the `helm-diff` plugin
/// makes, without needing the plugin. It writes nothing, so a protected
/// cluster may still be previewed.
pub async fn preview_upgrade(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<UpgradeDiff, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let helm = core.helm().map_err(|e| e.to_string())?;
    let kubeconfig = core
        .clusters
        .minified_kubeconfig(cluster, Some(&request.namespace))
        .map_err(|e| e.to_string())?;

    let proposed = helm
        .template(
            &handle,
            &kubeconfig,
            &request.target(),
            &request.chart_ref(),
            &request.values,
        )
        .await
        .map_err(|e| e.to_string())?;

    // A release that does not exist yet has no installed manifest, so the diff
    // is everything the install would create.
    let current = store::detail(&handle, &request.namespace, &request.release, None)
        .await
        .map(|detail| detail.manifest)
        .unwrap_or_default();

    Ok(cli::diff_manifests(&current, &proposed, &request.release))
}

/// Upgrade a release, installing it if it does not exist. Returns helm's output.
pub async fn upgrade(
    core: &AppCore,
    cluster: &str,
    request: &UpgradeRequest,
) -> Result<String, String> {
    let prepared = prepare(core, cluster, &request.namespace)?;
    let result = prepared
        .helm
        .upgrade(
            &prepared.handle,
            &prepared.kubeconfig,
            &request.target(),
            &request.chart_ref(),
            &request.values,
            &request.options,
        )
        .await;
    core.record_audit(
        cluster,
        "helmUpgrade",
        &format!("{}/{}", request.namespace, request.release),
        &result,
    );
    result.map_err(|e| e.to_string())
}
