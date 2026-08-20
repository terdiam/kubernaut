//! Terminals.
//!
//! Four ways in, all producing the same session type so the UI has one code
//! path:
//!
//! * **Pod exec** — a shell inside a container.
//! * **Ephemeral container** — a debug container attached to a running pod, for
//!   images that ship no shell at all.
//! * **Node shell** — a privileged pod that `nsenter`s into the node's own
//!   namespaces, for when the problem is below Kubernetes.
//! * **Local shell** — a shell on this machine with `KUBECONFIG` pinned to the
//!   cluster and namespace on screen.
//!
//! Output is forwarded as UTF-8 text for xterm.js. Reads are byte chunks that
//! can split a multi-byte character, so a partial tail is carried over to the
//! next chunk instead of being replaced with U+FFFD — otherwise any non-ASCII
//! output (box drawing, accented text, emoji) corrupts at random boundaries.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::SinkExt;
use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client,
    api::{AttachParams, DeleteParams, Patch, PatchParams, PostParams, TerminalSize},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{OpsError, Result},
    ring::Ring,
};

/// Terminal output chunks buffered before the oldest are dropped. A terminal
/// that scrolls faster than the UI renders is a runaway process, and the
/// interesting output is the newest.
const RING_CAPACITY: usize = 2_000;
const STDIN_CAPACITY: usize = 64;
/// How long to wait for a created debug pod or ephemeral container to start.
const START_TIMEOUT: Duration = Duration::from_secs(90);

/// Shell probe used when the caller does not supply a command. Prefers bash for
/// line editing but never fails on images that only ship `sh`.
const DEFAULT_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

/// Default image for debug containers and node shells. Pinned rather than
/// `latest` so a session cannot silently change behaviour between runs.
pub const DEFAULT_DEBUG_IMAGE: &str = "busybox:1.36";

// --------------------------------------------------------------- options

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOptions {
    pub namespace: String,
    pub pod: String,
    pub container: Option<String>,
    /// Empty means "open a shell".
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub columns: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralOptions {
    pub namespace: String,
    pub pod: String,
    /// Container whose process namespace to join, so `ps` sees the app.
    pub target_container: Option<String>,
    #[serde(default = "default_debug_image")]
    pub image: String,
    /// Must equal the pod name: an ephemeral container can never be removed.
    pub confirmation: String,
    #[serde(default)]
    pub columns: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeShellOptions {
    pub node: String,
    /// Namespace the debug pod is created in.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_debug_image")]
    pub image: String,
    /// Must equal the node name: this creates a privileged pod with full host
    /// access.
    pub confirmation: String,
    #[serde(default)]
    pub columns: u16,
    #[serde(default)]
    pub rows: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShellOptions {
    /// Namespace pinned into the generated kubeconfig.
    pub namespace: Option<String>,
    #[serde(default)]
    pub columns: u16,
    #[serde(default)]
    pub rows: u16,
}

fn default_debug_image() -> String {
    DEFAULT_DEBUG_IMAGE.to_string()
}

fn default_namespace() -> String {
    "default".to_string()
}

// --------------------------------------------------------------- session

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    /// stdout/stderr text, already merged in arrival order.
    Output {
        data: String,
    },
    /// The remote process exited.
    Closed {
        status: String,
    },
    Failed {
        message: String,
    },
    /// Progress while a debug pod or ephemeral container starts.
    Status {
        message: String,
    },
}

pub type SessionId = u64;

/// What the UI needs to label the tab and warn about side effects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDescriptor {
    pub session_id: SessionId,
    /// `podExec` | `ephemeral` | `nodeShell` | `localShell`
    pub kind: String,
    pub title: String,
    /// Shown once in the terminal header when the session leaves something
    /// behind or holds elevated privileges.
    pub warning: Option<String>,
}

/// Work to undo when a session ends.
enum Cleanup {
    None,
    /// Debug pods are created by us and must not outlive the terminal.
    DeletePod {
        client: Client,
        namespace: String,
        name: String,
    },
    /// The generated kubeconfig holds credentials; remove it promptly.
    RemoveFile(PathBuf),
}

pub struct TerminalSession {
    pub id: SessionId,
    pub descriptor: TerminalDescriptor,
    ring: Arc<Ring<TerminalEvent>>,
    stdin: mpsc::Sender<Vec<u8>>,
    resize: mpsc::Sender<(u16, u16)>,
    cancel: CancellationToken,
    cleanup: Mutex<Cleanup>,
}

impl TerminalSession {
    pub async fn next_batch(&self) -> Option<Vec<TerminalEvent>> {
        let (batch, dropped) = self.ring.next_batch().await?;
        if dropped > 0 {
            tracing::debug!(session = self.id, dropped, "terminal output truncated");
        }
        Some(batch)
    }

    /// Send keystrokes. Fails only once the session has ended.
    pub async fn write(&self, data: Vec<u8>) -> Result<()> {
        self.stdin
            .send(data)
            .await
            .map_err(|_| OpsError::UnknownSession(self.id.to_string()))
    }

    /// Tell the remote pty the window changed. Ignored when the target has no
    /// tty (a non-interactive command).
    pub async fn resize(&self, columns: u16, rows: u16) -> Result<()> {
        let _ = self.resize.send((columns, rows)).await;
        Ok(())
    }

    pub fn stop(&self) {
        self.cancel.cancel();
        self.ring.close();

        // Cleanup runs detached: `stop` is called from `Drop` and from command
        // handlers, neither of which can await.
        let action = std::mem::replace(&mut *self.cleanup.lock(), Cleanup::None);
        match action {
            Cleanup::None => {}
            Cleanup::DeletePod {
                client,
                namespace,
                name,
            } => {
                tokio::spawn(async move {
                    let api: Api<Pod> = Api::namespaced(client, &namespace);
                    // Grace period zero: the pod exists only for this session
                    // and waiting 30s to reap it just leaves a privileged pod
                    // running on the node.
                    let params = DeleteParams {
                        grace_period_seconds: Some(0),
                        ..DeleteParams::background()
                    };
                    match api.delete(&name, &params).await {
                        Ok(_) => tracing::info!(pod = %name, "debug pod removed"),
                        Err(err) => {
                            tracing::warn!(pod = %name, %err, "could not remove debug pod")
                        }
                    }
                });
            }
            Cleanup::RemoveFile(path) => {
                if let Err(err) = std::fs::remove_file(&path) {
                    tracing::warn!(path = %path.display(), %err, "could not remove temporary kubeconfig");
                }
            }
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
pub struct TerminalManager {
    next_id: AtomicU64,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> SessionId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // ------------------------------------------------------- pod exec

    pub async fn open(
        &self,
        cluster: &Arc<ClusterHandle>,
        options: ExecOptions,
    ) -> Result<Arc<TerminalSession>> {
        let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &options.namespace);

        let mut params = AttachParams::interactive_tty();
        if let Some(container) = &options.container {
            params = params.container(container);
        }

        let command: Vec<String> = if options.command.is_empty() {
            vec!["/bin/sh".into(), "-c".into(), DEFAULT_SHELL.into()]
        } else {
            options.command.clone()
        };

        let process = api.exec(&options.pod, command, &params).await?;

        Ok(self.wire(
            process,
            TerminalDescriptor {
                session_id: 0, // replaced in `wire`
                kind: "podExec".into(),
                title: options
                    .container
                    .clone()
                    .map(|c| format!("{}/{c}", options.pod))
                    .unwrap_or_else(|| options.pod.clone()),
                warning: None,
            },
            cluster,
            (options.columns, options.rows),
            Cleanup::None,
        ))
    }

    // --------------------------------------------- ephemeral container

    /// Attach a debug container to a running pod.
    ///
    /// Ephemeral containers cannot be removed or changed once added — the pod
    /// carries it until it is recreated. That is why this takes a confirmation.
    pub async fn open_ephemeral(
        &self,
        cluster: &Arc<ClusterHandle>,
        options: EphemeralOptions,
    ) -> Result<Arc<TerminalSession>> {
        if options.confirmation != options.pod {
            return Err(OpsError::other(format!(
                "confirmation `{}` does not match pod `{}`; nothing was changed",
                options.confirmation, options.pod
            )));
        }

        let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &options.namespace);
        let name = format!("kubernaut-debug-{}", short_suffix());

        let mut container = json!({
            "name": name,
            "image": options.image,
            "command": ["/bin/sh", "-c", DEFAULT_SHELL],
            "stdin": true,
            "tty": true,
            "terminationMessagePolicy": "File",
        });
        // Sharing the target's process namespace is what makes `ps` and
        // `/proc/1/root` useful; without it the debug container sees only
        // itself.
        if let Some(target) = &options.target_container {
            container["targetContainerName"] = json!(target);
        }

        let patch = json!({
            "spec": { "ephemeralContainers": [container] }
        });

        api.patch_ephemeral_containers(
            &options.pod,
            &PatchParams::default(),
            &Patch::Strategic(&patch),
        )
        .await?;

        let (session, parts) = self.wire_pending(
            cluster,
            TerminalDescriptor {
                session_id: 0,
                kind: "ephemeral".into(),
                title: format!("{} · {name}", options.pod),
                warning: Some(
                    "Ephemeral containers cannot be removed. This one stays on the pod until \
                     the pod is recreated."
                        .into(),
                ),
            },
            (options.columns, options.rows),
            Cleanup::None,
        );

        let attach = {
            let api = api.clone();
            let pod = options.pod.clone();
            let container_name = name.clone();
            move || async move {
                wait_for_ephemeral(&api, &pod, &container_name).await?;
                let params = AttachParams::interactive_tty().container(&container_name);
                Ok(api.attach(&pod, &params).await?)
            }
        };
        spawn_attach(session.clone(), parts, attach());
        Ok(session)
    }

    // ------------------------------------------------------ node shell

    /// A root shell in the node's own namespaces.
    ///
    /// Creates a privileged pod with host PID/network/IPC that `nsenter`s into
    /// PID 1. This is the same mechanism as `kubectl debug node/<name>`, and it
    /// is effectively root on the machine — hence the confirmation and the
    /// automatic teardown when the session closes.
    pub async fn open_node_shell(
        &self,
        cluster: &Arc<ClusterHandle>,
        options: NodeShellOptions,
    ) -> Result<Arc<TerminalSession>> {
        if options.confirmation != options.node {
            return Err(OpsError::other(format!(
                "confirmation `{}` does not match node `{}`; no pod was created",
                options.confirmation, options.node
            )));
        }

        let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &options.namespace);
        let name = format!("kubernaut-node-shell-{}", short_suffix());

        let manifest: Pod = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": name,
                "namespace": options.namespace,
                "labels": {
                    "app.kubernetes.io/managed-by": "kubernaut",
                    "kubernaut.dev/purpose": "node-shell",
                },
            },
            "spec": {
                "nodeName": options.node,
                "hostPID": true,
                "hostNetwork": true,
                "hostIPC": true,
                "restartPolicy": "Never",
                // Without a blanket toleration the pod will not schedule onto a
                // tainted node — which is exactly the node you need a shell on.
                "tolerations": [{ "operator": "Exists" }],
                "terminationGracePeriodSeconds": 0,
                "containers": [{
                    "name": "shell",
                    "image": options.image,
                    "securityContext": { "privileged": true },
                    "stdin": true,
                    "tty": true,
                    "command": [
                        "nsenter", "--target", "1",
                        "--mount", "--uts", "--ipc", "--net", "--pid",
                        "--", "sh", "-c", DEFAULT_SHELL
                    ],
                }],
            }
        }))?;

        api.create(&PostParams::default(), &manifest).await?;

        let (session, parts) = self.wire_pending(
            cluster,
            TerminalDescriptor {
                session_id: 0,
                kind: "nodeShell".into(),
                title: format!("node/{}", options.node),
                warning: Some(format!(
                    "Privileged pod `{name}` is running on {} with full host access. \
                     It is deleted when this terminal closes.",
                    options.node
                )),
            },
            (options.columns, options.rows),
            Cleanup::DeletePod {
                client: cluster.client.clone(),
                namespace: options.namespace.clone(),
                name: name.clone(),
            },
        );

        let attach = {
            let api = api.clone();
            let name = name.clone();
            move || async move {
                wait_for_running(&api, &name).await?;
                let params = AttachParams::interactive_tty().container("shell");
                Ok(api.attach(&name, &params).await?)
            }
        };
        spawn_attach(session.clone(), parts, attach());
        Ok(session)
    }

    // ----------------------------------------------------- local shell

    /// A shell on this machine with `KUBECONFIG` pinned to the open cluster.
    ///
    /// The generated kubeconfig contains only this one context, so a shell
    /// opened against staging cannot reach production by typing a different
    /// `--context`.
    pub async fn open_local_shell(
        &self,
        cluster: &Arc<ClusterHandle>,
        kubeconfig_yaml: String,
        options: LocalShellOptions,
    ) -> Result<Arc<TerminalSession>> {
        let path = k8s_core::kubeconfig::write_private(&kubeconfig_yaml)?;

        let id = self.next_id();
        let ring = Ring::new(RING_CAPACITY);
        let cancel = cluster.cancel_token().child_token();
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(STDIN_CAPACITY);
        let (resize_tx, resize_rx) = mpsc::channel::<(u16, u16)>(8);

        let descriptor = TerminalDescriptor {
            session_id: id,
            kind: "localShell".into(),
            title: format!("shell · {}", cluster.id),
            warning: Some(
                "This is a shell on your machine. KUBECONFIG points at a temporary file \
                 containing only this context; it is deleted when the terminal closes."
                    .into(),
            ),
        };

        let session = Arc::new(TerminalSession {
            id,
            descriptor,
            ring: ring.clone(),
            stdin: stdin_tx,
            resize: resize_tx,
            cancel: cancel.clone(),
            cleanup: Mutex::new(Cleanup::RemoveFile(path.clone())),
        });

        spawn_local_pty(
            path,
            cluster.id.clone(),
            options,
            ring,
            cancel,
            stdin_rx,
            resize_rx,
        );

        Ok(session)
    }

    // --------------------------------------------------------- wiring

    /// Build a session around an already-attached process.
    fn wire(
        &self,
        mut process: kube::api::AttachedProcess,
        mut descriptor: TerminalDescriptor,
        cluster: &Arc<ClusterHandle>,
        size: (u16, u16),
        cleanup: Cleanup,
    ) -> Arc<TerminalSession> {
        let id = self.next_id();
        descriptor.session_id = id;

        let ring = Ring::new(RING_CAPACITY);
        let cancel = cluster.cancel_token().child_token();
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(STDIN_CAPACITY);
        let (resize_tx, resize_rx) = mpsc::channel::<(u16, u16)>(8);

        wire_process(
            &mut process,
            ring.clone(),
            cancel.clone(),
            stdin_rx,
            resize_rx,
            size,
        );
        spawn_status(process, ring.clone(), cancel.clone());

        Arc::new(TerminalSession {
            id,
            descriptor,
            ring,
            stdin: stdin_tx,
            resize: resize_tx,
            cancel,
            cleanup: Mutex::new(cleanup),
        })
    }

    /// Build a session whose process is not attached yet, so the UI can show
    /// progress while a debug pod starts.
    fn wire_pending(
        &self,
        cluster: &Arc<ClusterHandle>,
        mut descriptor: TerminalDescriptor,
        size: (u16, u16),
        cleanup: Cleanup,
    ) -> (Arc<TerminalSession>, PendingParts) {
        let id = self.next_id();
        descriptor.session_id = id;

        let ring = Ring::new(RING_CAPACITY);
        let cancel = cluster.cancel_token().child_token();
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(STDIN_CAPACITY);
        let (resize_tx, resize_rx) = mpsc::channel::<(u16, u16)>(8);

        let session = Arc::new(TerminalSession {
            id,
            descriptor,
            ring,
            stdin: stdin_tx,
            resize: resize_tx,
            cancel,
            cleanup: Mutex::new(cleanup),
        });

        (
            session,
            PendingParts {
                stdin: stdin_rx,
                resize: resize_rx,
                size,
            },
        )
    }
}

/// Channel ends a session needs once its attach completes. Handed straight to
/// [`spawn_attach`] rather than parked in a registry, so there is no shared
/// state to leak if an attach fails.
struct PendingParts {
    stdin: mpsc::Receiver<Vec<u8>>,
    resize: mpsc::Receiver<(u16, u16)>,
    size: (u16, u16),
}

/// Await an attach future, then wire it into an already-created session.
fn spawn_attach<F>(session: Arc<TerminalSession>, parts: PendingParts, future: F)
where
    F: std::future::Future<Output = Result<kube::api::AttachedProcess>> + Send + 'static,
{
    session.ring.push(TerminalEvent::Status {
        message: "waiting for the container to start…".into(),
    });

    tokio::spawn(async move {
        match future.await {
            Ok(mut process) => {
                session.ring.push(TerminalEvent::Status {
                    message: "attached".into(),
                });
                wire_process(
                    &mut process,
                    session.ring.clone(),
                    session.cancel.clone(),
                    parts.stdin,
                    parts.resize,
                    parts.size,
                );
                spawn_status(process, session.ring.clone(), session.cancel.clone());
            }
            Err(err) => {
                session.ring.push(TerminalEvent::Failed {
                    message: err.to_string(),
                });
                session.ring.close();
            }
        }
    });
}

/// Connect a process's stdio and resize channel to the session plumbing.
fn wire_process(
    process: &mut kube::api::AttachedProcess,
    ring: Arc<Ring<TerminalEvent>>,
    cancel: CancellationToken,
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u16, u16)>,
    size: (u16, u16),
) {
    if let Some(mut sender) = process.terminal_size() {
        let cancel = cancel.clone();
        let initial = size;
        tokio::spawn(async move {
            // Set the initial window before anything is drawn, so a full-screen
            // TUI does not first paint at 80x24 and then reflow.
            if initial.0 > 0 && initial.1 > 0 {
                let _ = sender
                    .send(TerminalSize {
                        width: initial.0,
                        height: initial.1,
                    })
                    .await;
            }
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    size = resize_rx.recv() => match size {
                        Some((width, height)) => {
                            if sender.send(TerminalSize { width, height }).await.is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
    }

    if let Some(mut writer) = process.stdin() {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => return,
                    chunk = stdin_rx.recv() => match chunk {
                        Some(chunk) => chunk,
                        None => return,
                    },
                };
                if writer.write_all(&chunk).await.is_err() {
                    return;
                }
                let _ = writer.flush().await;
            }
        });
    }

    if let Some(reader) = process.stdout() {
        spawn_reader(reader, ring.clone(), cancel.clone());
    }
    if let Some(reader) = process.stderr() {
        spawn_reader(reader, ring, cancel);
    }
}

/// Own the attached process for the life of the session and report its exit.
///
/// The ownership part is not incidental: dropping an `AttachedProcess` aborts
/// the underlying websocket. Taking only its status future and letting the
/// process fall out of scope closes the connection the instant the terminal
/// opens, which looks exactly like a shell that exited immediately.
fn spawn_status(
    mut process: kube::api::AttachedProcess,
    ring: Arc<Ring<TerminalEvent>>,
    cancel: CancellationToken,
) {
    let status_future = process.take_status();
    tokio::spawn(async move {
        let status = tokio::select! {
            _ = cancel.cancelled() => {
                // The user closed the tab; tear the connection down rather
                // than leaving it open until the remote process happens to end.
                process.abort();
                return;
            }
            status = async {
                match status_future {
                    Some(future) => future.await,
                    None => None,
                }
            } => status,
        };

        let text = match status {
            Some(status) => status
                .message
                .or(status.reason)
                .unwrap_or_else(|| status.status.unwrap_or_else(|| "exited".into())),
            None => "session closed".into(),
        };
        ring.push(TerminalEvent::Closed { status: text });
        ring.close();
    });
}

fn spawn_reader<R>(reader: R, ring: Arc<Ring<TerminalEvent>>, cancel: CancellationToken)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let mut buf = vec![0u8; 8192];
        // Bytes of an incomplete UTF-8 sequence carried into the next read.
        let mut pending: Vec<u8> = Vec::new();

        loop {
            let read = tokio::select! {
                _ = cancel.cancelled() => return,
                read = reader.read(&mut buf) => read,
            };

            match read {
                Ok(0) => return,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    emit_text(&mut pending, &ring);
                }
                Err(err) => {
                    ring.push(TerminalEvent::Failed {
                        message: err.to_string(),
                    });
                    return;
                }
            }
        }
    });
}

/// Emit the valid UTF-8 prefix of `pending`, keeping any trailing partial
/// character for the next read.
fn emit_text(pending: &mut Vec<u8>, ring: &Arc<Ring<TerminalEvent>>) {
    match std::str::from_utf8(pending) {
        Ok(text) => {
            if !text.is_empty() {
                ring.push(TerminalEvent::Output { data: text.into() });
            }
            pending.clear();
        }
        Err(err) => {
            let valid = err.valid_up_to();
            if valid > 0 {
                let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                ring.push(TerminalEvent::Output { data: text });
            }
            // Keep only the trailing partial character. A long run of genuinely
            // invalid bytes would otherwise grow `pending` without bound.
            let tail = pending.split_off(valid);
            *pending = if tail.len() > 4 { Vec::new() } else { tail };
        }
    }
}

// ------------------------------------------------------------- waiting

async fn wait_for_running(api: &Api<Pod>, name: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(OpsError::other(format!(
                "pod `{name}` did not start within {}s",
                START_TIMEOUT.as_secs()
            )));
        }
        let pod = api.get(name).await?;
        let phase = pod
            .status
            .as_ref()
            .and_then(|s| s.phase.as_deref())
            .unwrap_or("");
        match phase {
            "Running" => return Ok(()),
            "Failed" | "Succeeded" => {
                return Err(OpsError::other(format!(
                    "pod `{name}` ended in phase {phase} before a shell could attach"
                )));
            }
            _ => {}
        }
        // Surface the scheduler's reason rather than a bare timeout: "no node
        // matches" and "still pulling the image" need different responses.
        if let Some(reason) = pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref())
            .and_then(|list| list.first())
            .and_then(|c| c.state.as_ref())
            .and_then(|s| s.waiting.as_ref())
            .and_then(|w| w.reason.as_deref())
            && matches!(
                reason,
                "ErrImagePull" | "ImagePullBackOff" | "InvalidImageName"
            )
        {
            return Err(OpsError::other(format!(
                "pod `{name}` cannot start: {reason}. Is the debug image reachable from this cluster?"
            )));
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
}

async fn wait_for_ephemeral(api: &Api<Pod>, pod: &str, container: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(OpsError::other(format!(
                "debug container `{container}` did not start within {}s",
                START_TIMEOUT.as_secs()
            )));
        }
        let object = api.get(pod).await?;
        if let Some(status) = object
            .status
            .as_ref()
            .and_then(|s| s.ephemeral_container_statuses.as_ref())
            .and_then(|list| list.iter().find(|c| c.name == container))
        {
            if status
                .state
                .as_ref()
                .and_then(|s| s.running.as_ref())
                .is_some()
            {
                return Ok(());
            }
            if let Some(terminated) = status.state.as_ref().and_then(|s| s.terminated.as_ref()) {
                return Err(OpsError::other(format!(
                    "debug container exited immediately ({})",
                    terminated
                        .reason
                        .clone()
                        .unwrap_or_else(|| "unknown".into())
                )));
            }
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
}

// --------------------------------------------------------- local shell

#[allow(clippy::too_many_arguments)]
fn spawn_local_pty(
    kubeconfig: PathBuf,
    context: String,
    options: LocalShellOptions,
    ring: Arc<Ring<TerminalEvent>>,
    cancel: CancellationToken,
    mut stdin_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u16, u16)>,
) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let columns = if options.columns > 0 {
        options.columns
    } else {
        80
    };
    let rows = if options.rows > 0 { options.rows } else { 24 };

    tokio::task::spawn_blocking(move || {
        let pty = native_pty_system();
        let pair = match pty.openpty(PtySize {
            rows,
            cols: columns,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(err) => {
                ring.push(TerminalEvent::Failed {
                    message: format!("could not allocate a pty: {err}"),
                });
                ring.close();
                return;
            }
        };

        let shell = default_login_shell();
        let mut command = CommandBuilder::new(&shell);
        command.env("KUBECONFIG", kubeconfig.as_os_str());
        command.env("KUBERNAUT_CONTEXT", &context);
        if let Some(namespace) = &options.namespace {
            command.env("KUBERNAUT_NAMESPACE", namespace);
        }
        command.env("TERM", "xterm-256color");
        if let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()) {
            command.cwd(home);
        }

        let mut child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(err) => {
                ring.push(TerminalEvent::Failed {
                    message: format!("could not start `{shell}`: {err}"),
                });
                ring.close();
                return;
            }
        };
        // The slave must be dropped or the master never sees EOF on exit.
        drop(pair.slave);

        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(err) => {
                ring.push(TerminalEvent::Failed {
                    message: err.to_string(),
                });
                ring.close();
                return;
            }
        };
        let writer = pair.master.take_writer().ok();
        let master = pair.master;

        // Writer and resize live on their own blocking task; the reader loop
        // below owns this one.
        let write_cancel = cancel.clone();
        std::thread::spawn(move || {
            let mut writer = writer;
            loop {
                if write_cancel.is_cancelled() {
                    return;
                }
                match stdin_rx.blocking_recv() {
                    Some(chunk) => {
                        if let Some(writer) = writer.as_mut() {
                            use std::io::Write;
                            if writer.write_all(&chunk).is_err() || writer.flush().is_err() {
                                return;
                            }
                        }
                    }
                    None => return,
                }
            }
        });

        let resize_cancel = cancel.clone();
        std::thread::spawn(move || {
            loop {
                if resize_cancel.is_cancelled() {
                    return;
                }
                match resize_rx.blocking_recv() {
                    Some((cols, rows)) if cols > 0 && rows > 0 => {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    Some(_) => {}
                    None => return,
                }
            }
        });

        let mut buf = vec![0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                break;
            }
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    emit_text(&mut pending, &ring);
                }
                Err(err) => {
                    ring.push(TerminalEvent::Failed {
                        message: err.to_string(),
                    });
                    break;
                }
            }
        }

        let status = child
            .wait()
            .map(|status| format!("shell exited ({status:?})"))
            .unwrap_or_else(|err| format!("shell ended: {err}"));
        ring.push(TerminalEvent::Closed { status });
        ring.close();
    });
}

fn default_login_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// Short random suffix for generated object names. Not security-sensitive; it
/// only has to avoid a collision between two open sessions.
fn short_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    use rand::RngExt;
    let mut rng = rand::rng();
    (0..6)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> Arc<Ring<TerminalEvent>> {
        Ring::new(16)
    }

    /// A multi-byte character split across two reads must not be mangled.
    #[test]
    fn split_utf8_sequences_are_rejoined() {
        let ring = ring();
        let text = "héllo";
        let bytes = text.as_bytes();
        // Split inside the two-byte `é`.
        let (head, tail) = bytes.split_at(2);

        let mut pending = head.to_vec();
        emit_text(&mut pending, &ring);
        pending.extend_from_slice(tail);
        emit_text(&mut pending, &ring);

        let (events, _) = ring.drain().unwrap();
        let joined: String = events
            .into_iter()
            .filter_map(|event| match event {
                TerminalEvent::Output { data } => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn invalid_bytes_do_not_grow_the_buffer_forever() {
        let ring = ring();
        let mut pending = vec![0xff; 64];
        emit_text(&mut pending, &ring);
        assert!(pending.len() <= 4, "kept {} bytes", pending.len());
    }

    #[test]
    fn suffixes_are_dns_safe_and_distinct() {
        let a = short_suffix();
        let b = short_suffix();
        assert_eq!(a.len(), 6);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_ne!(a, b, "two sessions would collide");
    }
}
