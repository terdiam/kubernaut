use std::{collections::HashMap, sync::Arc};

use k8s_core::{ClusterHandle, ClusterManager, ConnectOptions, SchemaCache, WatchManager};
use k8s_helm::Helm;

use crate::audit::AuditLog;
use crate::preferences::Preferences;
use crate::scan_cache::ScanCache;
use k8s_metrics::MetricsManager;
use k8s_ops::{
    exec::{SessionId as TerminalId, TerminalManager, TerminalSession},
    forward::ForwardManager,
    kustomize::Kustomize,
    logs::{LogManager, LogSession, SessionId as LogId},
};
use tokio::sync::Mutex;

/// Everything a front end shares. One instance per process.
pub struct AppCore {
    pub clusters: Arc<ClusterManager>,
    pub watches: Arc<WatchManager>,
    pub logs: Arc<LogManager>,
    pub terminals: Arc<TerminalManager>,
    pub forwards: Arc<ForwardManager>,
    pub schemas: Arc<SchemaCache>,
    pub metrics: Arc<MetricsManager>,
    pub audit: AuditLog,
    pub scan_cache: ScanCache,
    preferences: parking_lot::RwLock<Preferences>,
    /// Resolved lazily: helm may be absent, and that must not stop the app
    /// from starting or from listing releases (which needs no binary).
    helm: parking_lot::Mutex<Option<Arc<Helm>>>,
    /// Resolved lazily, same reasoning as `helm`.
    kustomize: parking_lot::Mutex<Option<Arc<Kustomize>>>,
    /// Directory holding the bundled sidecars, when the app was packaged.
    helm_sidecar_dir: parking_lot::Mutex<Option<std::path::PathBuf>>,
    /// Serialises read-modify-write of the preferences file.
    preferences_write: parking_lot::Mutex<()>,

    /// Live streaming sessions. Sessions stop themselves on drop, so removing
    /// the handle is what ends the stream.
    log_sessions: Mutex<HashMap<LogId, Arc<LogSession>>>,
    terminal_sessions: Mutex<HashMap<TerminalId, Arc<TerminalSession>>>,
}

impl AppCore {
    pub fn new(clusters: ClusterManager) -> Self {
        Self {
            clusters: Arc::new(clusters),
            watches: Arc::new(WatchManager::new()),
            logs: Arc::new(LogManager::new()),
            terminals: Arc::new(TerminalManager::new()),
            forwards: Arc::new(ForwardManager::new()),
            schemas: Arc::new(SchemaCache::new()),
            metrics: Arc::new(MetricsManager::new()),
            audit: AuditLog::new(),
            scan_cache: ScanCache::new(),
            preferences: parking_lot::RwLock::new(Preferences::load()),
            helm: parking_lot::Mutex::new(None),
            kustomize: parking_lot::Mutex::new(None),
            helm_sidecar_dir: parking_lot::Mutex::new(None),
            preferences_write: parking_lot::Mutex::new(()),
            log_sessions: Mutex::new(HashMap::new()),
            terminal_sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Connect to a context using the options saved for it (impersonation,
    /// default namespace, proxy), or return the existing handle.
    pub async fn connect(&self, context: &str) -> k8s_core::Result<Arc<ClusterHandle>> {
        let profile = self.preferences().profile(context);
        let options = ConnectOptions {
            impersonate_user: profile.impersonate_user,
            impersonate_groups: profile.impersonate_groups,
            default_namespace: profile.default_namespace,
            accept_invalid_certs: profile.accept_invalid_certs,
            proxy_url: profile.proxy_url,
        };
        self.clusters.connect(context, options).await
    }

    /// Drop everything tied to a cluster, then its connection.
    pub async fn disconnect(&self, cluster: &str) {
        self.watches.drop_cluster(cluster);
        self.close_cluster_sessions(cluster).await;
        self.clusters.disconnect(cluster);
    }

    pub fn preferences(&self) -> Preferences {
        self.preferences.read().clone()
    }

    /// The Details cards that start folded, without copying every preference.
    pub fn details_folded(&self) -> Vec<String> {
        self.preferences.read().details_folded()
    }

    /// Whether an object is pinned, without copying every preference to ask:
    /// the drawer asks this on each frame it is drawn.
    pub fn is_pinned(&self, pin: &crate::preferences::PinnedObject) -> bool {
        self.preferences.read().is_pinned(pin)
    }

    /// Change one preference and save the file.
    ///
    /// Edits are applied to a fresh copy under a lock, so two settings changed
    /// at nearly the same moment both survive instead of the later one writing
    /// back a stale copy of the earlier. The lock is held across the file write
    /// and never across an `.await`.
    pub fn update_preferences(&self, edit: impl FnOnce(&mut Preferences)) -> Result<(), String> {
        let _serial = self.preferences_write.lock();
        let mut next = self.preferences();
        edit(&mut next);
        next.save()?;
        *self.preferences.write() = next;
        Ok(())
    }

    /// Remember the namespaces a cluster is narrowed to.
    pub fn remember_namespaces(&self, context: &str, namespaces: &[String]) -> Result<(), String> {
        self.update_preferences(|preferences| preferences.remember_namespaces(context, namespaces))
    }

    pub fn set_preferences(&self, preferences: Preferences) -> Result<(), String> {
        let _serial = self.preferences_write.lock();
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

    /// The kustomize binary, resolved on first use. Shares the same bundled
    /// sidecar directory as helm and trivy.
    pub fn kustomize(&self) -> k8s_ops::error::Result<Arc<Kustomize>> {
        if let Some(existing) = self.kustomize.lock().clone() {
            return Ok(existing);
        }
        let dir = self.helm_sidecar_dir.lock().clone();
        let kustomize = Arc::new(Kustomize::resolve(dir.as_deref())?);
        *self.kustomize.lock() = Some(kustomize.clone());
        Ok(kustomize)
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
