use std::{collections::HashMap, ops::Deref};

use k8s_app::AppCore;
use k8s_core::{ClusterManager, watch::SubscriptionId};
use tauri::async_runtime::JoinHandle;
use tokio::sync::Mutex;

/// Everything the commands share. One instance, managed by Tauri.
///
/// The shared state lives in [`AppCore`]; this adds only what is specific to
/// pumping data across Tauri's IPC, and derefs to the core so commands read
/// `state.clusters` as before.
pub struct AppState {
    core: AppCore,
    /// Forwarder task per watch subscription: broadcast receiver → IPC channel.
    /// Kept so `stop_watch` can abort the task instead of leaving it parked on
    /// a receiver that will never resolve.
    watch_forwarders: Mutex<HashMap<SubscriptionId, JoinHandle<()>>>,
}

impl Deref for AppState {
    type Target = AppCore;

    fn deref(&self) -> &AppCore {
        &self.core
    }
}

impl AppState {
    pub fn new(clusters: ClusterManager) -> Self {
        Self {
            core: AppCore::new(clusters),
            watch_forwarders: Mutex::new(HashMap::new()),
        }
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
}
