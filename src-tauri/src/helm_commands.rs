//! Helm: releases, repositories and chart operations.
//!
//! Listing and inspection read the cluster's release Secrets directly, so they
//! work with no helm binary present. Only the operations that render charts
//! shell out.

use k8s_helm::{
    ChartResult, HelmInfo, Release, ReleaseDetail, ReleaseRevision, Repository, UpgradeDiff,
    UpgradeOptions,
    cli::{self, ChartRef, ReleaseTarget},
    store,
};
use serde::Deserialize;
use tauri::State;

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

/// Confirmation guard for the operations that destroy or replace a release.
fn require_confirmation(expected: &str, given: &str) -> CommandResult<()> {
    if expected == given {
        return Ok(());
    }
    Err(CommandError::new(format!(
        "confirmation `{given}` does not match `{expected}`; nothing was changed"
    )))
}

#[tauri::command]
pub async fn helm_info(state: State<'_, AppState>) -> CommandResult<Option<HelmInfo>> {
    // Absence is a normal state, not an error: everything read-only still
    // works, so the UI shows a hint rather than a failure.
    let Ok(helm) = state.helm() else {
        return Ok(None);
    };
    Ok(helm.info().await.ok())
}

#[tauri::command]
pub async fn helm_releases(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
) -> CommandResult<Vec<Release>> {
    let handle = state.clusters.require(&cluster)?;
    store::list(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_history(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    name: String,
) -> CommandResult<Vec<ReleaseRevision>> {
    let handle = state.clusters.require(&cluster)?;
    store::history(&handle, &namespace, &name)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_release_detail(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    name: String,
    revision: Option<i64>,
) -> CommandResult<ReleaseDetail> {
    let handle = state.clusters.require(&cluster)?;
    store::detail(&handle, &namespace, &name, revision)
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------ repositories

#[tauri::command]
pub async fn helm_repos(state: State<'_, AppState>) -> CommandResult<Vec<Repository>> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.repo_list().await.map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_repo_add(
    state: State<'_, AppState>,
    name: String,
    url: String,
) -> CommandResult<()> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.repo_add(&name, &url).await.map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_repo_remove(state: State<'_, AppState>, name: String) -> CommandResult<()> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.repo_remove(&name).await.map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_repo_update(state: State<'_, AppState>) -> CommandResult<String> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.repo_update().await.map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_search(
    state: State<'_, AppState>,
    query: String,
) -> CommandResult<Vec<ChartResult>> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.search(&query).await.map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_chart_values(
    state: State<'_, AppState>,
    chart: String,
    version: Option<String>,
) -> CommandResult<String> {
    let helm = state.helm().map_err(CommandError::new)?;
    helm.show_values(&chart, version.as_deref())
        .await
        .map_err(CommandError::new)
}

// ---------------------------------------------------------------- releases

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeRequest {
    pub cluster: String,
    pub namespace: String,
    pub release: String,
    /// `repo/chart`, a local path, or an OCI reference.
    pub chart: String,
    pub version: Option<String>,
    /// Values YAML from the editor.
    pub values: String,
    #[serde(default)]
    pub options: UpgradeOptions,
}

/// What an upgrade would change, without changing it.
///
/// Renders the proposed chart and diffs it against the manifest helm recorded
/// for the current revision — the same comparison the `helm-diff` plugin makes,
/// without needing the plugin.
#[tauri::command]
pub async fn helm_preview_upgrade(
    state: State<'_, AppState>,
    request: UpgradeRequest,
) -> CommandResult<UpgradeDiff> {
    let handle = state.clusters.require(&request.cluster)?;
    let helm = state.helm().map_err(CommandError::new)?;
    let kubeconfig = state
        .clusters
        .minified_kubeconfig(&request.cluster, Some(&request.namespace))?;

    let proposed = helm
        .template(
            &handle,
            &kubeconfig,
            &ReleaseTarget {
                release: &request.release,
                namespace: &request.namespace,
            },
            &ChartRef {
                reference: &request.chart,
                version: request.version.as_deref(),
            },
            &request.values,
        )
        .await
        .map_err(CommandError::new)?;

    // A release that does not exist yet has no installed manifest; the diff is
    // then simply everything the install would create.
    let current = store::detail(&handle, &request.namespace, &request.release, None)
        .await
        .map(|detail| detail.manifest)
        .unwrap_or_default();

    Ok(cli::diff_manifests(&current, &proposed, &request.release))
}

#[tauri::command]
pub async fn helm_upgrade(
    state: State<'_, AppState>,
    request: UpgradeRequest,
) -> CommandResult<String> {
    state
        .ensure_writable(&request.cluster)
        .map_err(CommandError::new)?;
    let handle = state.clusters.require(&request.cluster)?;
    let helm = state.helm().map_err(CommandError::new)?;
    let kubeconfig = state
        .clusters
        .minified_kubeconfig(&request.cluster, Some(&request.namespace))?;

    helm.upgrade(
        &handle,
        &kubeconfig,
        &ReleaseTarget {
            release: &request.release,
            namespace: &request.namespace,
        },
        &ChartRef {
            reference: &request.chart,
            version: request.version.as_deref(),
        },
        &request.values,
        &request.options,
    )
    .await
    .map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_rollback(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    release: String,
    revision: i64,
    confirmation: String,
) -> CommandResult<String> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    require_confirmation(&release, &confirmation)?;
    let handle = state.clusters.require(&cluster)?;
    let helm = state.helm().map_err(CommandError::new)?;
    let kubeconfig = state
        .clusters
        .minified_kubeconfig(&cluster, Some(&namespace))?;

    helm.rollback(&handle, &kubeconfig, &release, &namespace, revision)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn helm_uninstall(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    release: String,
    confirmation: String,
    keep_history: Option<bool>,
) -> CommandResult<String> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    require_confirmation(&release, &confirmation)?;
    let handle = state.clusters.require(&cluster)?;
    let helm = state.helm().map_err(CommandError::new)?;
    let kubeconfig = state
        .clusters
        .minified_kubeconfig(&cluster, Some(&namespace))?;

    helm.uninstall(
        &handle,
        &kubeconfig,
        &release,
        &namespace,
        keep_history.unwrap_or(false),
    )
    .await
    .map_err(CommandError::new)
}
