//! Log streaming, including multi-pod tail for a whole workload.
//!
//! `kubectl logs deploy/x` tails one pod. Here a workload target tails *every*
//! matching pod at once and keeps following as pods come and go during a
//! rollout, which is the case where reading logs actually matters.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::{AsyncBufReadExt, StreamExt};
use k8s_core::cluster::ClusterHandle;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, ResourceExt,
    api::{DynamicObject, ListParams, LogParams},
    runtime::{WatchStreamExt, watcher},
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{OpsError, Result},
    ring::Ring,
};

/// Lines buffered before the oldest are dropped. ~5k lines is a couple of
/// screens of scrollback per pod and a few MB at most.
const RING_CAPACITY: usize = 5_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogOptions {
    pub container: Option<String>,
    #[serde(default = "default_true")]
    pub follow: bool,
    /// Lines of history to fetch before following.
    pub tail_lines: Option<i64>,
    pub since_seconds: Option<i64>,
    #[serde(default)]
    pub timestamps: bool,
    /// Read the previous container instance — the only way to see why a
    /// CrashLoopBackOff pod died.
    #[serde(default)]
    pub previous: bool,
}

fn default_true() -> bool {
    true
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            container: None,
            follow: true,
            tail_lines: Some(500),
            since_seconds: None,
            timestamps: false,
            previous: false,
        }
    }
}

impl LogOptions {
    fn to_params(&self, container: Option<String>) -> LogParams {
        LogParams {
            container: container.or_else(|| self.container.clone()),
            follow: self.follow,
            tail_lines: self.tail_lines,
            since_seconds: self.since_seconds,
            timestamps: self.timestamps,
            previous: self.previous,
            limit_bytes: None,
            pretty: false,
            since_time: None,
        }
    }
}

/// What to tail.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LogTarget {
    /// A single pod (optionally one container of it).
    Pod { namespace: String, name: String },
    /// Every pod behind a workload or service, followed as the set changes.
    Workload {
        namespace: String,
        /// `group/version/plural` of the owning object.
        resource: String,
        name: String,
    },
}

impl LogTarget {
    pub fn namespace(&self) -> &str {
        match self {
            Self::Pod { namespace, .. } | Self::Workload { namespace, .. } => namespace,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LogEvent {
    Line {
        pod: String,
        container: String,
        text: String,
    },
    /// Emitted when the ring evicted lines the UI never saw.
    Dropped { count: u64 },
    /// A pod's stream finished (pod deleted, or `follow: false` completed).
    PodEnded { pod: String, reason: String },
    /// A pod's stream failed; other pods in the session keep running.
    PodFailed { pod: String, message: String },
}

pub type SessionId = u64;

pub struct LogSession {
    pub id: SessionId,
    ring: Arc<Ring<LogEvent>>,
    cancel: CancellationToken,
    /// Pods currently being tailed, so the pod watcher does not start a second
    /// stream for a pod it already follows.
    tailing: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl LogSession {
    /// Next batch of events, with dropped lines folded in as a marker.
    pub async fn next_batch(&self) -> Option<Vec<LogEvent>> {
        let (mut batch, dropped) = self.ring.next_batch().await?;
        if dropped > 0 {
            batch.insert(0, LogEvent::Dropped { count: dropped });
        }
        Some(batch)
    }

    pub fn stop(&self) {
        self.cancel.cancel();
        self.ring.close();
        for (_, token) in self.tailing.lock().drain() {
            token.cancel();
        }
    }
}

impl Drop for LogSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Containers available on a pod, used to populate the container picker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfo {
    pub name: String,
    /// `init` | `app` | `ephemeral`
    pub role: String,
    pub image: String,
    pub ready: bool,
    pub restarts: i32,
    /// `running` | `waiting` | `terminated` | `unknown`
    pub state: String,
    /// Waiting or terminated reason, whichever applies.
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// The parts of a container's state the log view can fall back on.
///
/// A pod outlives its log files: the kubelet garbage-collects dead containers
/// and their `/var/log/pods` directory long before the object leaves etcd. When
/// the logs are gone this is all the forensics that remain, so it travels with
/// the container list rather than needing a second round trip at the moment the
/// log fetch fails.
#[derive(Debug, Clone, Default)]
struct ContainerState {
    state: String,
    reason: Option<String>,
    exit_code: Option<i32>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

fn container_state(status: Option<&k8s_openapi::api::core::v1::ContainerStatus>) -> ContainerState {
    let Some(status) = status else {
        return ContainerState {
            state: "unknown".into(),
            ..Default::default()
        };
    };
    let state = status.state.as_ref();

    if let Some(terminated) = state.and_then(|s| s.terminated.as_ref()) {
        return ContainerState {
            state: "terminated".into(),
            reason: terminated.reason.clone(),
            exit_code: Some(terminated.exit_code),
            started_at: terminated.started_at.as_ref().map(|t| t.0.to_string()),
            finished_at: terminated.finished_at.as_ref().map(|t| t.0.to_string()),
        };
    }
    if let Some(waiting) = state.and_then(|s| s.waiting.as_ref()) {
        return ContainerState {
            state: "waiting".into(),
            reason: waiting.reason.clone(),
            ..Default::default()
        };
    }
    if let Some(running) = state.and_then(|s| s.running.as_ref()) {
        return ContainerState {
            state: "running".into(),
            started_at: running.started_at.as_ref().map(|t| t.0.to_string()),
            ..Default::default()
        };
    }
    ContainerState {
        state: "unknown".into(),
        ..Default::default()
    }
}

pub async fn containers(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    pod: &str,
) -> Result<Vec<ContainerInfo>> {
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    let pod = api.get(pod).await?;
    Ok(describe_containers(&pod))
}

fn describe_containers(pod: &Pod) -> Vec<ContainerInfo> {
    let status = pod.status.as_ref();
    let lookup = |name: &str, init: bool| {
        let list = status.and_then(|s| {
            if init {
                s.init_container_statuses.as_ref()
            } else {
                s.container_statuses.as_ref()
            }
        });
        let found = list.and_then(|l| l.iter().find(|c| c.name == name));
        let (ready, restarts) = found
            .map(|c| (c.ready, c.restart_count))
            .unwrap_or((false, 0));
        (ready, restarts, container_state(found))
    };

    let spec = match &pod.spec {
        Some(spec) => spec,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for c in spec.init_containers.iter().flatten() {
        let (ready, restarts, state) = lookup(&c.name, true);
        out.push(ContainerInfo {
            name: c.name.clone(),
            role: "init".into(),
            image: c.image.clone().unwrap_or_default(),
            ready,
            restarts,
            state: state.state,
            reason: state.reason,
            exit_code: state.exit_code,
            started_at: state.started_at,
            finished_at: state.finished_at,
        });
    }
    for c in &spec.containers {
        let (ready, restarts, state) = lookup(&c.name, false);
        out.push(ContainerInfo {
            name: c.name.clone(),
            role: "app".into(),
            image: c.image.clone().unwrap_or_default(),
            ready,
            restarts,
            state: state.state,
            reason: state.reason,
            exit_code: state.exit_code,
            started_at: state.started_at,
            finished_at: state.finished_at,
        });
    }
    for c in spec.ephemeral_containers.iter().flatten() {
        out.push(ContainerInfo {
            name: c.name.clone(),
            role: "ephemeral".into(),
            image: c.image.clone().unwrap_or_default(),
            ready: false,
            restarts: 0,
            state: "unknown".into(),
            reason: None,
            exit_code: None,
            started_at: None,
            finished_at: None,
        });
    }
    out
}

/// Owns every open log session.
#[derive(Default)]
pub struct LogManager {
    next_id: AtomicU64,
}

impl LogManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tailing. The returned session streams until dropped or stopped.
    pub async fn start(
        &self,
        cluster: &Arc<ClusterHandle>,
        target: LogTarget,
        options: LogOptions,
    ) -> Result<Arc<LogSession>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ring = Ring::new(RING_CAPACITY);
        let cancel = cluster.cancel_token().child_token();
        let tailing = Arc::new(Mutex::new(HashMap::new()));

        let session = Arc::new(LogSession {
            id,
            ring: ring.clone(),
            cancel: cancel.clone(),
            tailing: tailing.clone(),
        });

        match target {
            LogTarget::Pod { namespace, name } => {
                spawn_pod_tail(
                    cluster.clone(),
                    namespace,
                    name,
                    options,
                    ring,
                    cancel,
                    tailing,
                );
            }
            LogTarget::Workload {
                namespace,
                resource,
                name,
            } => {
                let selector = pod_selector(cluster, &resource, &namespace, &name).await?;
                spawn_workload_tail(
                    cluster.clone(),
                    namespace,
                    selector,
                    options,
                    ring,
                    cancel,
                    tailing,
                );
            }
        }

        Ok(session)
    }
}

/// Resolve a workload's pod selector into a label selector string.
///
/// Deployments/StatefulSets/DaemonSets/Jobs use `.spec.selector.matchLabels`;
/// Services use the flat `.spec.selector`. Anything else has no pod set.
async fn pod_selector(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Result<String> {
    let obj: DynamicObject =
        k8s_core::objects::get(cluster, resource, Some(namespace), name).await?;
    let kind = obj
        .types
        .as_ref()
        .map(|t| t.kind.clone())
        .unwrap_or_else(|| resource.to_string());

    let spec = obj.data.get("spec");
    let labels = spec
        .and_then(|s| s.get("selector"))
        .and_then(|s| s.get("matchLabels").or(Some(s)))
        .and_then(|s| s.as_object())
        .ok_or(OpsError::NoSelector { kind })?;

    let selector = labels
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(",");

    if selector.is_empty() {
        return Err(OpsError::NoSelector {
            kind: resource.to_string(),
        });
    }
    Ok(selector)
}

type Tailing = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// Follow the pod set for a selector, tailing pods as they appear and stopping
/// when they go away. This is what keeps logs flowing across a rollout.
fn spawn_workload_tail(
    cluster: Arc<ClusterHandle>,
    namespace: String,
    selector: String,
    options: LogOptions,
    ring: Arc<Ring<LogEvent>>,
    cancel: CancellationToken,
    tailing: Tailing,
) {
    tokio::spawn(async move {
        let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &namespace);
        let config = watcher::Config::default().labels(&selector).any_semantic();
        let mut stream = watcher(api, config).default_backoff().boxed();

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                event = stream.next() => match event {
                    Some(event) => event,
                    None => break,
                },
            };

            match event {
                Ok(watcher::Event::Apply(pod) | watcher::Event::InitApply(pod)) => {
                    let name = pod.name_any();
                    // Pending pods have no log stream yet; the watcher will
                    // deliver them again once they start.
                    let started = pod
                        .status
                        .as_ref()
                        .and_then(|s| s.phase.as_deref())
                        .is_some_and(|p| p != "Pending");
                    if !started || tailing.lock().contains_key(&name) {
                        continue;
                    }
                    spawn_pod_tail(
                        cluster.clone(),
                        namespace.clone(),
                        name,
                        options.clone(),
                        ring.clone(),
                        cancel.clone(),
                        tailing.clone(),
                    );
                }
                Ok(watcher::Event::Delete(pod)) => {
                    let name = pod.name_any();
                    if let Some(token) = tailing.lock().remove(&name) {
                        token.cancel();
                    }
                    ring.push(LogEvent::PodEnded {
                        pod: name,
                        reason: "pod deleted".into(),
                    });
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::debug!(%err, "pod watch for log session failed; retrying");
                }
            }
        }
        ring.close();
    });
}

fn spawn_pod_tail(
    cluster: Arc<ClusterHandle>,
    namespace: String,
    pod: String,
    options: LogOptions,
    ring: Arc<Ring<LogEvent>>,
    parent_cancel: CancellationToken,
    tailing: Tailing,
) {
    let token = parent_cancel.child_token();
    tailing.lock().insert(pod.clone(), token.clone());

    tokio::spawn(async move {
        let api: Api<Pod> = Api::namespaced(cluster.client.clone(), &namespace);

        // With no container named, tail every app container in the pod so a
        // sidecar's output is not silently missing.
        let containers: Vec<Option<String>> = match &options.container {
            Some(c) => vec![Some(c.clone())],
            None => match api.get(&pod).await {
                Ok(spec) => {
                    let names: Vec<String> = describe_containers(&spec)
                        .into_iter()
                        .filter(|c| c.role == "app")
                        .map(|c| c.name)
                        .collect();
                    if names.len() <= 1 {
                        vec![names.into_iter().next()]
                    } else {
                        names.into_iter().map(Some).collect()
                    }
                }
                Err(err) => {
                    ring.push(LogEvent::PodFailed {
                        pod: pod.clone(),
                        message: err.to_string(),
                    });
                    tailing.lock().remove(&pod);
                    return;
                }
            },
        };

        let mut handles = Vec::new();
        for container in containers {
            let api = api.clone();
            let pod_name = pod.clone();
            let ring = ring.clone();
            let token = token.clone();
            let params = options.to_params(container.clone());
            let label = container.clone().unwrap_or_else(|| pod.clone());

            handles.push(tokio::spawn(async move {
                stream_container(api, pod_name, label, params, ring, token).await;
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
        tailing.lock().remove(&pod);
    });
}

async fn stream_container(
    api: Api<Pod>,
    pod: String,
    container: String,
    params: LogParams,
    ring: Arc<Ring<LogEvent>>,
    cancel: CancellationToken,
) {
    let stream = match api.log_stream(&pod, &params).await {
        Ok(stream) => stream,
        Err(err) => {
            ring.push(LogEvent::PodFailed {
                pod,
                message: err.to_string(),
            });
            return;
        }
    };

    // kube yields a `futures::AsyncBufRead`, so this is a line *stream*, not
    // tokio's `next_line()`.
    let mut lines = std::pin::pin!(stream.lines());
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => return,
            line = lines.next() => line,
        };

        match line {
            Some(Ok(text)) => ring.push(LogEvent::Line {
                pod: pod.clone(),
                container: container.clone(),
                text,
            }),
            None => {
                ring.push(LogEvent::PodEnded {
                    pod,
                    reason: "stream closed".into(),
                });
                return;
            }
            Some(Err(err)) => {
                ring.push(LogEvent::PodFailed {
                    pod,
                    message: err.to_string(),
                });
                return;
            }
        }
    }
}

/// One-shot fetch, used by "download logs".
pub async fn snapshot(
    cluster: &Arc<ClusterHandle>,
    namespace: &str,
    pod: &str,
    options: &LogOptions,
) -> Result<String> {
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    let mut params = options.to_params(None);
    params.follow = false;
    Ok(api.logs(pod, &params).await?)
}

/// Pods currently matching a workload, for the pod picker in the log toolbar.
pub async fn workload_pods(
    cluster: &Arc<ClusterHandle>,
    resource: &str,
    namespace: &str,
    name: &str,
) -> Result<Vec<String>> {
    let selector = pod_selector(cluster, resource, namespace, name).await?;
    let api: Api<Pod> = Api::namespaced(cluster.client.clone(), namespace);
    let list = api
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(OpsError::from)?;
    Ok(list.iter().map(|p| p.name_any()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn container_status_survives_the_logs_and_reaches_the_ui_by_name() {
        // The log view falls back on these fields when the log file is gone,
        // so the wire names it reads must not drift.
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "cleanup-1", "namespace": "production" },
            "spec": {
                "initContainers": [{ "name": "wait", "image": "busybox" }],
                "containers": [{ "name": "cleanup", "image": "cleanup:1" }]
            },
            "status": {
                "initContainerStatuses": [{
                    "name": "wait", "image": "busybox", "ready": true, "restartCount": 0,
                    "state": { "terminated": { "exitCode": 0, "reason": "Completed", "finishedAt": "2026-06-07T04:00:19Z" } }
                }],
                "containerStatuses": [{
                    "name": "cleanup", "image": "cleanup:1", "ready": false, "restartCount": 2,
                    "state": { "terminated": {
                        "exitCode": 1, "reason": "Error",
                        "startedAt": "2026-06-07T04:00:20Z",
                        "finishedAt": "2026-06-07T04:01:08Z"
                    } }
                }]
            }
        }))
        .expect("pod fixture");

        let described = describe_containers(&pod);
        // Init containers come first, which is why the log view must not take
        // `containers[0]` as the one to report on.
        assert_eq!(described[0].name, "wait");
        assert_eq!(described[0].role, "init");

        let app = &described[1];
        assert_eq!(app.state, "terminated");
        assert_eq!(app.exit_code, Some(1));
        assert_eq!(app.reason.as_deref(), Some("Error"));
        assert_eq!(app.restarts, 2);

        let json = serde_json::to_value(app).expect("serialise");
        for key in ["state", "reason", "exitCode", "startedAt", "finishedAt"] {
            assert!(json.get(key).is_some(), "`{key}` missing from {json}");
        }
        assert_eq!(json["startedAt"], "2026-06-07T04:00:20Z");
    }

    #[test]
    fn a_waiting_container_reports_its_reason_and_no_exit_code() {
        let pod: Pod = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": { "name": "api-1", "namespace": "app" },
            "spec": { "containers": [{ "name": "api", "image": "api:1" }] },
            "status": {
                "containerStatuses": [{
                    "name": "api", "image": "api:1", "ready": false, "restartCount": 0,
                    "state": { "waiting": { "reason": "ImagePullBackOff" } }
                }]
            }
        }))
        .expect("pod fixture");

        let described = describe_containers(&pod);
        assert_eq!(described[0].state, "waiting");
        assert_eq!(described[0].reason.as_deref(), Some("ImagePullBackOff"));
        assert_eq!(described[0].exit_code, None);
    }
}
