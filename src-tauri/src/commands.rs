//! The IPC surface. Every cluster credential stays on this side of the bridge:
//! the frontend only ever sees names, rows and rendered YAML.

use std::sync::Arc;

use k8s_core::{
    ClusterStatus, ConnectOptions, ContextEntry, DiscoveryCache, objects,
    watch::{SubscriptionId, WatchBatch, WatchRequest, WatchState},
};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State, ipc::Channel};

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

/// Payload of the `cluster://status` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterStatusEvent {
    pub cluster: String,
    pub status: ClusterStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSummary {
    pub id: String,
    pub default_namespace: String,
    pub status: ClusterStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchHandle {
    pub subscription_id: SubscriptionId,
    pub spec: k8s_core::TableSpec,
    /// Contents at subscribe time so the table paints immediately.
    pub initial: WatchBatch,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectPayload {
    pub yaml: String,
    pub json: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    pub cluster: String,
    pub resource: String,
    pub namespace: Option<String>,
    pub name: String,
}

#[tauri::command]
pub fn list_contexts(state: State<'_, AppState>) -> Vec<ContextEntry> {
    state.clusters.contexts()
}

#[tauri::command]
pub fn reload_kubeconfig(state: State<'_, AppState>) -> CommandResult<Vec<ContextEntry>> {
    Ok(state.clusters.reload_kubeconfig()?)
}

#[tauri::command]
pub async fn connect_cluster(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    context: String,
    options: Option<ConnectOptions>,
) -> CommandResult<ClusterSummary> {
    // Stored per-cluster options (impersonation, default namespace, proxy)
    // apply unless the caller overrides them for this connection.
    let profile = state.preferences().profile(&context);
    let options = options.unwrap_or(ConnectOptions {
        impersonate_user: profile.impersonate_user,
        impersonate_groups: profile.impersonate_groups,
        default_namespace: profile.default_namespace,
        accept_invalid_certs: profile.accept_invalid_certs,
        proxy_url: profile.proxy_url,
    });

    let handle = state.clusters.connect(&context, options).await?;

    // Stream later status changes so a VPN drop shows up without polling.
    let mut rx = handle.watch_status();
    let cluster = handle.id.clone();
    let cancel = handle.cancel_token();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                changed = rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let status = rx.borrow_and_update().clone();
                    let _ = app.emit("cluster://status", ClusterStatusEvent {
                        cluster: cluster.clone(),
                        status,
                    });
                }
            }
        }
    });

    Ok(ClusterSummary {
        id: handle.id.clone(),
        default_namespace: handle.default_namespace.clone(),
        status: handle.status(),
    })
}

#[tauri::command]
pub async fn disconnect_cluster(state: State<'_, AppState>, cluster: String) -> CommandResult<()> {
    state.watches.drop_cluster(&cluster);
    state.abort_all_forwarders().await;
    state.close_cluster_sessions(&cluster).await;
    state.clusters.disconnect(&cluster);
    Ok(())
}

#[tauri::command]
pub fn cluster_status(state: State<'_, AppState>, cluster: String) -> CommandResult<ClusterStatus> {
    Ok(state.clusters.require(&cluster)?.status())
}

/// Discovery for one cluster. `refresh` forces a re-run, which is how newly
/// installed CRDs appear without reconnecting.
#[tauri::command]
pub async fn discover(
    state: State<'_, AppState>,
    cluster: String,
    refresh: Option<bool>,
) -> CommandResult<Arc<DiscoveryCache>> {
    let handle = state.clusters.require(&cluster)?;
    if refresh.unwrap_or(false) {
        return Ok(handle.refresh_discovery().await?);
    }
    match handle.discovery() {
        Some(cache) => Ok(cache),
        None => Ok(handle.refresh_discovery().await?),
    }
}

/// JSON Schema for a resource type, used by the YAML editor and the form
/// builder. Cached per cluster because the OpenAPI document is large.
#[tauri::command]
pub async fn resource_schema(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
) -> CommandResult<Arc<k8s_core::ResourceSchema>> {
    let handle = state.clusters.require(&cluster)?;
    Ok(state.schemas.get(&handle, &resource).await?)
}

#[tauri::command]
pub async fn list_namespaces(
    state: State<'_, AppState>,
    cluster: String,
) -> CommandResult<Vec<String>> {
    let handle = state.clusters.require(&cluster)?;
    Ok(objects::list_namespaces(&handle).await)
}

/// Start (or join) a watch and stream batches into `channel`.
#[tauri::command]
pub async fn watch_resource(
    state: State<'_, AppState>,
    cluster: String,
    request: WatchRequest,
    channel: Channel<Arc<WatchBatch>>,
) -> CommandResult<WatchHandle> {
    let handle = state.clusters.require(&cluster)?;
    let subscription = state.watches.subscribe(&handle, request).await?;

    let id = subscription.id;
    let spec = subscription.spec.clone();
    let initial = subscription.initial.clone();
    let mut receiver = subscription.receiver;
    let watches = state.watches.clone();

    let forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(batch) => {
                    if channel.send(batch).is_err() {
                        // Frontend closed the channel (view unmounted).
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // The UI fell behind a burst. Deltas are gone, so push a
                    // full snapshot rather than leaving the table wrong.
                    tracing::debug!(
                        subscription = id,
                        skipped,
                        "watch subscriber lagged; resyncing"
                    );
                    if let Some(snapshot) = watches.resync(id)
                        && channel.send(Arc::new(snapshot)).is_err()
                    {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    state.register_forwarder(id, forwarder).await;

    Ok(WatchHandle {
        subscription_id: id,
        spec,
        initial,
    })
}

#[tauri::command]
pub async fn stop_watch(
    state: State<'_, AppState>,
    subscription_id: SubscriptionId,
) -> CommandResult<()> {
    state.abort_forwarder(subscription_id).await;
    state.watches.unsubscribe(subscription_id);
    Ok(())
}

/// Explicit resync, for the "reload" button in the table header.
#[tauri::command]
pub fn resync_watch(
    state: State<'_, AppState>,
    subscription_id: SubscriptionId,
) -> Option<WatchBatch> {
    state.watches.resync(subscription_id)
}

#[tauri::command]
pub async fn get_object(
    state: State<'_, AppState>,
    reference: ObjectRef,
    include_managed_fields: Option<bool>,
) -> CommandResult<ObjectPayload> {
    let handle = state.clusters.require(&reference.cluster)?;
    let obj = objects::get(
        &handle,
        &reference.resource,
        reference.namespace.as_deref(),
        &reference.name,
    )
    .await?;

    let include = include_managed_fields.unwrap_or(false);
    let mut json = objects::to_value(&obj)?;
    if !include {
        // The form editor round-trips this value straight back into an apply,
        // which the apiserver rejects if it still carries managedFields.
        objects::strip_managed_fields(&mut json);
    }

    Ok(ObjectPayload {
        yaml: objects::to_yaml(&obj, include)?,
        json,
    })
}

// ----------------------------------------------------- cluster imports

/// Contexts available in the user's own kubeconfig, for the import picker.
///
/// Listing is not adding: nothing is reachable until a context is picked and
/// copied into this app's storage.
#[tauri::command]
pub fn system_kubeconfig_contexts() -> Vec<ContextEntry> {
    k8s_core::kubeconfig::system_contexts()
}

/// Copy chosen contexts out of the system kubeconfig into this app.
#[tauri::command]
pub fn import_system_contexts(
    state: State<'_, AppState>,
    contexts: Vec<String>,
    label: String,
) -> CommandResult<Vec<ContextEntry>> {
    let source = k8s_core::kubeconfig::read_system()?;
    let yaml = k8s_core::kubeconfig::extract(&source, &contexts)?;

    crate::clusters::import(&crate::clusters::ImportRequest {
        yaml,
        label,
        renames: Default::default(),
    })
    .map_err(CommandError::new)?;

    Ok(state
        .clusters
        .set_managed(crate::clusters::managed_files())?)
}

/// Per-cluster display name, colour and connection options.
#[tauri::command]
pub fn cluster_profile(
    state: State<'_, AppState>,
    context: String,
) -> crate::preferences::ClusterProfile {
    state.preferences().profile(&context)
}

#[tauri::command]
pub fn set_cluster_profile(
    state: State<'_, AppState>,
    context: String,
    profile: crate::preferences::ClusterProfile,
) -> CommandResult<crate::preferences::Preferences> {
    let mut preferences = state.preferences();
    preferences.cluster_profiles.insert(context, profile);
    state
        .set_preferences(preferences)
        .map_err(CommandError::new)?;
    Ok(state.preferences())
}

#[tauri::command]
pub fn managed_kubeconfigs() -> Vec<crate::clusters::ManagedKubeconfig> {
    crate::clusters::list()
}

/// What importing this document would add, and what it would clash with.
#[tauri::command]
pub fn preview_kubeconfig(
    state: State<'_, AppState>,
    yaml: String,
) -> CommandResult<crate::clusters::ImportPreview> {
    let existing: Vec<String> = state
        .clusters
        .contexts()
        .into_iter()
        .map(|context| context.name)
        .collect();
    crate::clusters::preview(&yaml, &existing).map_err(CommandError::new)
}

/// Read a kubeconfig the user picked from disk.
///
/// The file is read here rather than in the frontend so the renderer never
/// needs filesystem permission, and so credentials are only ever handled on
/// this side of the bridge.
#[tauri::command]
pub fn read_kubeconfig_file(path: String) -> CommandResult<String> {
    std::fs::read_to_string(&path)
        .map_err(|err| CommandError::new(format!("could not read `{path}`: {err}")))
}

/// Import a kubeconfig into this app's own directory.
#[tauri::command]
pub fn import_kubeconfig(
    state: State<'_, AppState>,
    request: crate::clusters::ImportRequest,
) -> CommandResult<Vec<ContextEntry>> {
    crate::clusters::import(&request).map_err(CommandError::new)?;
    Ok(state
        .clusters
        .set_managed(crate::clusters::managed_files())?)
}

/// Forget an imported kubeconfig. The cluster itself is untouched.
#[tauri::command]
pub fn remove_kubeconfig(
    state: State<'_, AppState>,
    file: String,
) -> CommandResult<Vec<ContextEntry>> {
    crate::clusters::remove(&file).map_err(CommandError::new)?;
    Ok(state
        .clusters
        .set_managed(crate::clusters::managed_files())?)
}

/// A panic recorded in an earlier run, if there is one.
#[tauri::command]
pub fn last_crash() -> Option<crate::logging::CrashReport> {
    crate::logging::last_crash()
}

#[tauri::command]
pub fn get_preferences(state: State<'_, AppState>) -> crate::preferences::Preferences {
    state.preferences()
}

/// Save preferences and apply the ones that take effect immediately.
#[tauri::command]
pub async fn set_preferences(
    state: State<'_, AppState>,
    preferences: crate::preferences::Preferences,
) -> CommandResult<crate::preferences::Preferences> {
    let extra = preferences.extra_path_entries.clone();
    state
        .set_preferences(preferences)
        .map_err(CommandError::new)?;

    // PATH changes matter to exec credential plugins, which run per connection,
    // so re-applying now is enough — no restart needed.
    k8s_core::paths::hydrate_process_path(&extra).await;
    Ok(state.preferences())
}

/// Diagnostics shown in the About/Troubleshooting pane.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub version: String,
    pub kubeconfig_paths: Vec<String>,
    pub path_entries: Vec<String>,
    pub active_watches: usize,
    pub connected_clusters: Vec<String>,
    /// Where preferences are stored, so a user can inspect or remove the file.
    pub preferences_path: Option<String>,
    /// Where local logs are written. Nothing is ever sent anywhere.
    pub log_directory: Option<String>,
}

#[tauri::command]
pub fn diagnostics(state: State<'_, AppState>) -> Diagnostics {
    Diagnostics {
        version: env!("CARGO_PKG_VERSION").to_string(),
        kubeconfig_paths: k8s_core::kubeconfig::kubeconfig_paths()
            .into_iter()
            .map(|p| p.display().to_string())
            .collect(),
        path_entries: std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .map(|p| p.display().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        active_watches: state.watches.active_watches(),
        connected_clusters: state.clusters.connected(),
        preferences_path: state.preferences().path(),
        log_directory: crate::logging::log_directory().map(|path| path.display().to_string()),
    }
}

/// Surface a watch's current state without waiting for the next batch.
#[tauri::command]
pub fn watch_state(
    state: State<'_, AppState>,
    subscription_id: SubscriptionId,
) -> Option<WatchState> {
    state.watches.resync(subscription_id).map(|b| b.state)
}

impl From<serde_json::Error> for CommandError {
    fn from(err: serde_json::Error) -> Self {
        CommandError::new(err)
    }
}
