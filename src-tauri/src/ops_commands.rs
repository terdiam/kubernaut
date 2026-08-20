//! IPC surface for cluster operations: logs, terminals, forwards, edits and
//! actions. Streaming commands return a session id and push events into an IPC
//! channel; the frontend closes the channel to stop them.

use k8s_ops::{
    actions::{self, DeleteRequest, DrainOptions, DrainReport, TargetRef},
    apply::{self, ApplyOutcome, DiffResult, EditRequest},
    diagnose::{self, DiagnosisReport},
    exec::{
        EphemeralOptions, ExecOptions, LocalShellOptions, NodeShellOptions,
        SessionId as TerminalId, TerminalDescriptor, TerminalEvent, TerminalSession,
    },
    forward::{ForwardId, ForwardSpec, ForwardStatus, PortOption},
    gitops::{self, GitOpsSummary},
    logs::{self, ContainerInfo, LogEvent, LogOptions, LogTarget, SessionId as LogId},
    manifest::{self, DocResult, ManifestPlan},
    related::{self, EventRow, Related},
};
use serde::Serialize;
use tauri::{State, ipc::Channel};

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHandle {
    pub session_id: u64,
}

// ---------------------------------------------------------------- logs

#[tauri::command]
pub async fn start_logs(
    state: State<'_, AppState>,
    cluster: String,
    target: LogTarget,
    options: Option<LogOptions>,
    channel: Channel<Vec<LogEvent>>,
) -> CommandResult<SessionHandle> {
    let handle = state.clusters.require(&cluster)?;
    let session = state
        .logs
        .start(&handle, target, options.unwrap_or_default())
        .await
        .map_err(CommandError::new)?;

    let id = session.id;
    state.register_log_session(session.clone()).await;

    tauri::async_runtime::spawn(async move {
        while let Some(batch) = session.next_batch().await {
            if channel.send(batch).is_err() {
                break; // view closed
            }
        }
    });

    Ok(SessionHandle { session_id: id })
}

#[tauri::command]
pub async fn stop_logs(state: State<'_, AppState>, session_id: LogId) -> CommandResult<()> {
    if let Some(session) = state.take_log_session(session_id).await {
        session.stop();
    }
    Ok(())
}

#[tauri::command]
pub async fn pod_containers(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    pod: String,
) -> CommandResult<Vec<ContainerInfo>> {
    let handle = state.clusters.require(&cluster)?;
    logs::containers(&handle, &namespace, &pod)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn workload_pods(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: String,
    name: String,
) -> CommandResult<Vec<String>> {
    let handle = state.clusters.require(&cluster)?;
    logs::workload_pods(&handle, &resource, &namespace, &name)
        .await
        .map_err(CommandError::new)
}

/// Whole log as text, for "download" / "copy all".
#[tauri::command]
pub async fn log_snapshot(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    pod: String,
    options: Option<LogOptions>,
) -> CommandResult<String> {
    let handle = state.clusters.require(&cluster)?;
    logs::snapshot(&handle, &namespace, &pod, &options.unwrap_or_default())
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------ terminal

/// Register a session and start pumping its output into the IPC channel.
async fn attach_session(
    state: &State<'_, AppState>,
    session: std::sync::Arc<TerminalSession>,
    channel: Channel<Vec<TerminalEvent>>,
) -> TerminalDescriptor {
    let descriptor = session.descriptor.clone();
    state.register_terminal(session.clone()).await;

    tauri::async_runtime::spawn(async move {
        while let Some(batch) = session.next_batch().await {
            if channel.send(batch).is_err() {
                break; // view closed
            }
        }
    });

    descriptor
}

/// A shell inside a container of a running pod.
#[tauri::command]
pub async fn open_terminal(
    state: State<'_, AppState>,
    cluster: String,
    options: ExecOptions,
    channel: Channel<Vec<TerminalEvent>>,
) -> CommandResult<TerminalDescriptor> {
    let handle = state.clusters.require(&cluster)?;
    let session = state
        .terminals
        .open(&handle, options)
        .await
        .map_err(CommandError::new)?;
    Ok(attach_session(&state, session, channel).await)
}

/// A debug container attached to a running pod, for images with no shell.
///
/// Writes to the cluster and cannot be undone, so `options.confirmation` must
/// equal the pod name.
#[tauri::command]
pub async fn open_ephemeral_terminal(
    state: State<'_, AppState>,
    cluster: String,
    options: EphemeralOptions,
    channel: Channel<Vec<TerminalEvent>>,
) -> CommandResult<TerminalDescriptor> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    let session = state
        .terminals
        .open_ephemeral(&handle, options)
        .await
        .map_err(CommandError::new)?;
    Ok(attach_session(&state, session, channel).await)
}

/// A root shell in a node's own namespaces, via a privileged debug pod that is
/// removed when the session closes. `options.confirmation` must equal the node
/// name.
#[tauri::command]
pub async fn open_node_shell(
    state: State<'_, AppState>,
    cluster: String,
    options: NodeShellOptions,
    channel: Channel<Vec<TerminalEvent>>,
) -> CommandResult<TerminalDescriptor> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    let session = state
        .terminals
        .open_node_shell(&handle, options)
        .await
        .map_err(CommandError::new)?;
    Ok(attach_session(&state, session, channel).await)
}

/// A shell on this machine with `KUBECONFIG` pinned to the open cluster.
#[tauri::command]
pub async fn open_local_shell(
    state: State<'_, AppState>,
    cluster: String,
    options: LocalShellOptions,
    channel: Channel<Vec<TerminalEvent>>,
) -> CommandResult<TerminalDescriptor> {
    let handle = state.clusters.require(&cluster)?;
    // Only this context is written to disk — see `kubeconfig::minified`.
    let kubeconfig = state
        .clusters
        .minified_kubeconfig(&cluster, options.namespace.as_deref())?;
    let session = state
        .terminals
        .open_local_shell(&handle, kubeconfig, options)
        .await
        .map_err(CommandError::new)?;
    Ok(attach_session(&state, session, channel).await)
}

#[tauri::command]
pub async fn terminal_write(
    state: State<'_, AppState>,
    session_id: TerminalId,
    data: String,
) -> CommandResult<()> {
    let session = state
        .terminal(session_id)
        .await
        .ok_or_else(|| CommandError::new("terminal session has closed"))?;
    session
        .write(data.into_bytes())
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    session_id: TerminalId,
    columns: u16,
    rows: u16,
) -> CommandResult<()> {
    // A resize arriving after the session ended is normal (the UI unmounts
    // asynchronously) and is not worth surfacing as an error.
    if let Some(session) = state.terminal(session_id).await {
        session
            .resize(columns, rows)
            .await
            .map_err(CommandError::new)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn close_terminal(
    state: State<'_, AppState>,
    session_id: TerminalId,
) -> CommandResult<()> {
    if let Some(session) = state.take_terminal(session_id).await {
        session.stop();
    }
    Ok(())
}

// ------------------------------------------------------- port forwards

#[tauri::command]
pub async fn start_forward(
    state: State<'_, AppState>,
    cluster: String,
    spec: ForwardSpec,
) -> CommandResult<ForwardStatus> {
    let handle = state.clusters.require(&cluster)?;
    state
        .forwards
        .start(&handle, spec)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub fn stop_forward(state: State<'_, AppState>, id: ForwardId) {
    state.forwards.stop(id);
}

#[tauri::command]
pub fn list_forwards(state: State<'_, AppState>) -> Vec<ForwardStatus> {
    state.forwards.list()
}

#[tauri::command]
pub async fn target_ports(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: String,
    name: String,
) -> CommandResult<Vec<PortOption>> {
    let handle = state.clusters.require(&cluster)?;
    k8s_ops::forward::target_ports(&handle, &resource, &namespace, &name)
        .await
        .map_err(CommandError::new)
}

// -------------------------------------------------------------- edits

#[tauri::command]
pub async fn preview_edit(
    state: State<'_, AppState>,
    cluster: String,
    request: EditRequest,
) -> CommandResult<DiffResult> {
    let handle = state.clusters.require(&cluster)?;
    apply::preview(&handle, &request)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn apply_edit(
    state: State<'_, AppState>,
    cluster: String,
    request: EditRequest,
) -> CommandResult<ApplyOutcome> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    apply::apply(&handle, &request)
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------ actions

#[tauri::command]
pub async fn scale_workload(
    state: State<'_, AppState>,
    cluster: String,
    target: TargetRef,
    replicas: i32,
) -> CommandResult<i32> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::scale(&handle, &target, replicas)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn current_scale(
    state: State<'_, AppState>,
    cluster: String,
    target: TargetRef,
) -> CommandResult<i32> {
    let handle = state.clusters.require(&cluster)?;
    actions::current_scale(&handle, &target)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn restart_workload(
    state: State<'_, AppState>,
    cluster: String,
    target: TargetRef,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::restart(&handle, &target)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn set_node_cordoned(
    state: State<'_, AppState>,
    cluster: String,
    node: String,
    cordoned: bool,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::set_cordoned(&handle, &node, cordoned)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn drain_node(
    state: State<'_, AppState>,
    cluster: String,
    node: String,
    options: DrainOptions,
) -> CommandResult<DrainReport> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::drain(&handle, &node, &options)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn delete_object(
    state: State<'_, AppState>,
    cluster: String,
    request: DeleteRequest,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::delete(&handle, &request)
        .await
        .map_err(CommandError::new)
}

#[tauri::command]
pub async fn evict_pod(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    name: String,
    confirmation: String,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    actions::evict_pod(&handle, &namespace, &name, &confirmation)
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------ context

/// Events about one object, oldest first.
#[tauri::command]
pub async fn object_events(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
    name: String,
) -> CommandResult<Vec<EventRow>> {
    let handle = state.clusters.require(&cluster)?;
    related::events(&handle, namespace.as_deref(), &name)
        .await
        .map_err(CommandError::new)
}

/// Events about a set of pods, for a workload's "recent events" panel.
#[tauri::command]
pub async fn pod_events(
    state: State<'_, AppState>,
    cluster: String,
    namespace: String,
    pods: Vec<String>,
) -> CommandResult<Vec<EventRow>> {
    let handle = state.clusters.require(&cluster)?;
    related::events_for_pods(&handle, &namespace, &pods)
        .await
        .map_err(CommandError::new)
}

/// Pods, services, ingresses, config, storage and policies connected to an
/// object.
#[tauri::command]
pub async fn related_resources(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: Option<String>,
    name: String,
) -> CommandResult<Related> {
    let handle = state.clusters.require(&cluster)?;
    related::related(&handle, &resource, namespace.as_deref(), &name)
        .await
        .map_err(CommandError::new)
}

/// Why a pod is not running, and the steps that follow from it.
///
/// Accepts a workload as well as a pod: the question is nearly always asked of
/// a Deployment, and the answer lives in its replicas.
#[tauri::command]
pub async fn diagnose_object(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: Option<String>,
    name: String,
) -> CommandResult<DiagnosisReport> {
    let handle = state.clusters.require(&cluster)?;
    diagnose::diagnose(&handle, &resource, namespace.as_deref(), &name)
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------ manifests

/// What applying a manifest would do, per document. Never writes.
#[tauri::command]
pub async fn plan_manifest(
    state: State<'_, AppState>,
    cluster: String,
    yaml: String,
    namespace: Option<String>,
    force: Option<bool>,
) -> CommandResult<ManifestPlan> {
    let handle = state.clusters.require(&cluster)?;
    manifest::plan(&handle, &yaml, namespace.as_deref(), force.unwrap_or(false))
        .await
        .map_err(CommandError::new)
}

/// Apply every document in a manifest, creating what does not exist yet.
#[tauri::command]
pub async fn apply_manifest(
    state: State<'_, AppState>,
    cluster: String,
    yaml: String,
    namespace: Option<String>,
    force: Option<bool>,
) -> CommandResult<Vec<DocResult>> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    manifest::apply(&handle, &yaml, namespace.as_deref(), force.unwrap_or(false))
        .await
        .map_err(CommandError::new)
}

// ------------------------------------------------------------- gitops

/// What the installed GitOps controllers are managing.
#[tauri::command]
pub async fn gitops_survey(
    state: State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
) -> CommandResult<GitOpsSummary> {
    let handle = state.clusters.require(&cluster)?;
    gitops::survey(&handle, namespace.as_deref())
        .await
        .map_err(CommandError::new)
}

/// Ask a controller to reconcile now. Writes an annotation to the object.
#[tauri::command]
pub async fn gitops_reconcile(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: Option<String>,
    name: String,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    gitops::reconcile(&handle, &resource, namespace.as_deref(), &name)
        .await
        .map_err(CommandError::new)
}

/// Pause or resume reconciliation.
#[tauri::command]
pub async fn gitops_set_suspended(
    state: State<'_, AppState>,
    cluster: String,
    resource: String,
    namespace: Option<String>,
    name: String,
    suspended: bool,
) -> CommandResult<()> {
    state.ensure_writable(&cluster).map_err(CommandError::new)?;
    let handle = state.clusters.require(&cluster)?;
    gitops::set_suspended(&handle, &resource, namespace.as_deref(), &name, suspended)
        .await
        .map_err(CommandError::new)
}
