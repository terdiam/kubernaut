use std::{collections::HashMap, sync::Arc};

use k8s_core::{ClusterManager, SchemaCache, WatchManager, watch::SubscriptionId};
use k8s_helm::Helm;

use crate::preferences::Preferences;
use k8s_metrics::MetricsManager;
use k8s_ops::{
    exec::{SessionId as TerminalId, TerminalManager, TerminalSession},
    forward::ForwardManager,
    logs::{LogManager, LogSession, SessionId as LogId},
};
use tauri::async_runtime::JoinHandle;
use tokio::sync::Mutex;

/// Everything the commands share. One instance, managed by Tauri.
pub struct AppState {
    pub clusters: Arc<ClusterManager>,
    pub watches: Arc<WatchManager>,
    pub logs: Arc<LogManager>,
    pub terminals: Arc<TerminalManager>,
    pub forwards: Arc<ForwardManager>,
    pub schemas: Arc<SchemaCache>,
    pub metrics: Arc<MetricsManager>,
    preferences: parking_lot::RwLock<Preferences>,
    /// Resolved lazily: helm may be absent, and that must not stop the app
    /// from starting or from listing releases (which needs no binary).
    helm: parking_lot::Mutex<Option<Arc<Helm>>>,
    /// Directory holding the bundled sidecars, when the app was packaged.
    helm_sidecar_dir: parking_lot::Mutex<Option<std::path::PathBuf>>,

    /// Forwarder task per watch subscription: broadcast receiver → IPC channel.
    /// Kept so `stop_watch` can abort the task instead of leaving it parked on
    /// a receiver that will never resolve.
    watch_forwarders: Mutex<HashMap<SubscriptionId, JoinHandle<()>>>,
    /// Live streaming sessions. Sessions stop themselves on drop, so removing
    /// the handle is what ends the stream.
    log_sessions: Mutex<HashMap<LogId, Arc<LogSession>>>,
    terminal_sessions: Mutex<HashMap<TerminalId, Arc<TerminalSession>>>,
}

impl AppState {
    pub fn new(clusters: ClusterManager) -> Self {
        Self {
            clusters: Arc::new(clusters),
            watches: Arc::new(WatchManager::new()),
            logs: Arc::new(LogManager::new()),
            terminals: Arc::new(TerminalManager::new()),
            forwards: Arc::new(ForwardManager::new()),
            schemas: Arc::new(SchemaCache::new()),
            metrics: Arc::new(MetricsManager::new()),
            preferences: parking_lot::RwLock::new(Preferences::load()),
            helm: parking_lot::Mutex::new(None),
            helm_sidecar_dir: parking_lot::Mutex::new(None),
            watch_forwarders: Mutex::new(HashMap::new()),
            log_sessions: Mutex::new(HashMap::new()),
            terminal_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn preferences(&self) -> Preferences {
        self.preferences.read().clone()
    }

    pub fn set_preferences(&self, preferences: Preferences) -> Result<(), String> {
        preferences.save()?;
        *self.preferences.write() = preferences;
        Ok(())
    }

    /// Refuse an action against a context the user marked protected.
    ///
    /// A confirmation dialog stops accidents but not habit — after the tenth
    /// one, people type the name without reading. This is the setting for
    /// clusters where the answer should simply be no.
    pub fn ensure_writable(&self, cluster: &str) -> Result<(), String> {
        if self.preferences.read().is_protected(cluster) {
            return Err(format!(
                "`{cluster}` is marked protected in Settings, so changes from this app are                  refused. Remove it from the protected list to allow them."
            ));
        }
        Ok(())
    }

    /// Point sidecar resolution at the packaged binaries directory.
    pub fn set_helm_sidecar_dir(&self, dir: Option<std::path::PathBuf>) {
        *self.helm_sidecar_dir.lock() = dir;
    }

    pub fn sidecar_dir(&self) -> Option<std::path::PathBuf> {
        self.helm_sidecar_dir.lock().clone()
    }

    /// The helm binary, resolved on first use.
    pub fn helm(&self) -> k8s_helm::Result<Arc<Helm>> {
        if let Some(existing) = self.helm.lock().clone() {
            return Ok(existing);
        }
        let dir = self.helm_sidecar_dir.lock().clone();
        let helm = Arc::new(Helm::resolve(dir.as_deref())?);
        *self.helm.lock() = Some(helm.clone());
        Ok(helm)
    }

    pub async fn register_forwarder(&self, id: SubscriptionId, handle: JoinHandle<()>) {
        self.watch_forwarders.lock().await.insert(id, handle);
    }

    pub async fn abort_forwarder(&self, id: SubscriptionId) {
        if let Some(handle) = self.watch_forwarders.lock().await.remove(&id) {
            handle.abort();
        }
    }

    pub async fn abort_all_forwarders(&self) {
        for (_, handle) in self.watch_forwarders.lock().await.drain() {
            handle.abort();
        }
    }

    pub async fn register_log_session(&self, session: Arc<LogSession>) {
        self.log_sessions.lock().await.insert(session.id, session);
    }

    pub async fn take_log_session(&self, id: LogId) -> Option<Arc<LogSession>> {
        self.log_sessions.lock().await.remove(&id)
    }

    pub async fn register_terminal(&self, session: Arc<TerminalSession>) {
        self.terminal_sessions
            .lock()
            .await
            .insert(session.id, session);
    }

    pub async fn terminal(&self, id: TerminalId) -> Option<Arc<TerminalSession>> {
        self.terminal_sessions.lock().await.get(&id).cloned()
    }

    pub async fn take_terminal(&self, id: TerminalId) -> Option<Arc<TerminalSession>> {
        self.terminal_sessions.lock().await.remove(&id)
    }

    /// Drop every session tied to a cluster, so nothing keeps a dead client
    /// alive after a disconnect.
    pub async fn close_cluster_sessions(&self, cluster: &str) {
        self.forwards.stop_cluster(cluster);
        self.schemas.clear_cluster(cluster);
        self.metrics.drop_cluster(cluster);
        if let Some(helm) = self.helm.lock().clone() {
            helm.forget_cluster(cluster);
        }
        self.log_sessions.lock().await.clear();
        self.terminal_sessions.lock().await.clear();
    }
}
