//! Shared, ref-counted watches with batched delta delivery.
//!
//! Two properties matter here:
//!
//! * **Sharing** — several views (table, hotbar badge, topology) often watch the
//!   same resource. They share one apiserver watch instead of opening three.
//! * **Batching** — a busy namespace can emit hundreds of events per second.
//!   Forwarding each one across the IPC boundary would starve the UI thread, so
//!   deltas are coalesced per uid over a 100 ms window before being sent.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dashmap::DashMap;
use futures::StreamExt;
use kube::{
    Api,
    api::DynamicObject,
    runtime::{reflector, watcher},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch as watch_channel};
use tokio_util::sync::CancellationToken;

use crate::{
    cluster::ClusterHandle,
    error::{CoreError, Result},
    row::{Row, RowProjector, TableSpec},
};

/// How long deltas are coalesced before being pushed to subscribers.
const BATCH_WINDOW: Duration = Duration::from_millis(100);
/// Batches buffered per subscriber before it is considered lagging.
const BROADCAST_CAPACITY: usize = 64;
/// A watch with no subscribers is kept alive this long, so switching tabs back
/// and forth does not re-list the whole resource.
const IDLE_GRACE: Duration = Duration::from_secs(30);

/// What the caller wants to watch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchRequest {
    /// `group/version/plural`, see [`crate::discovery::resource_key`].
    pub resource: String,
    /// `None` means all namespaces (or the resource is cluster-scoped).
    pub namespace: Option<String>,
    pub label_selector: Option<String>,
    pub field_selector: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WatchKey {
    cluster: String,
    request: WatchRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum WatchState {
    /// Initial list in flight.
    Initializing,
    Ready,
    /// The watch is retrying internally; rows on screen are stale but valid.
    Error {
        message: String,
    },
}

/// A coalesced set of changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchBatch {
    /// Bumped whenever the watch re-lists. The UI discards batches from an
    /// older epoch, which prevents a slow delta from resurrecting a stale row.
    pub epoch: u64,
    /// True when `upserts` is the complete contents, not a delta.
    pub snapshot: bool,
    pub upserts: Vec<Row>,
    /// uids removed since the previous batch.
    pub deletes: Vec<String>,
    pub state: WatchState,
}

pub type SubscriptionId = u64;

struct WatchEntry {
    store: reflector::Store<DynamicObject>,
    projector: Arc<RowProjector>,
    spec: TableSpec,
    tx: broadcast::Sender<Arc<WatchBatch>>,
    state: watch_channel::Receiver<WatchState>,
    epoch: Arc<AtomicU64>,
    subscribers: Arc<AtomicU64>,
    cancel: CancellationToken,
}

impl WatchEntry {
    /// Current contents of the reflector store as rows.
    fn snapshot(&self) -> WatchBatch {
        let rows = self
            .store
            .state()
            .iter()
            .map(|obj| self.projector.project(obj))
            .collect();
        WatchBatch {
            epoch: self.epoch.load(Ordering::Acquire),
            snapshot: true,
            upserts: rows,
            deletes: Vec::new(),
            state: self.state.borrow().clone(),
        }
    }
}

/// Handle returned to a caller; dropping it does **not** end the watch, callers
/// must call [`WatchManager::unsubscribe`] (the Tauri layer does this when the
/// frontend closes its channel).
pub struct Subscription {
    pub id: SubscriptionId,
    pub spec: TableSpec,
    /// Contents at subscribe time, so the UI can paint before the first delta.
    pub initial: WatchBatch,
    pub receiver: broadcast::Receiver<Arc<WatchBatch>>,
}

/// Owns every live watch across every cluster.
#[derive(Default)]
pub struct WatchManager {
    // Arc, not a bare DashMap: the idle-shutdown task needs a handle to the
    // *same* map. Cloning a DashMap deep-copies it, which would silently leak
    // every retired watch.
    entries: Arc<DashMap<WatchKey, Arc<WatchEntry>>>,
    subscriptions: DashMap<SubscriptionId, WatchKey>,
    next_id: AtomicU64,
}

impl WatchManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to a resource, starting the underlying watch if needed.
    pub async fn subscribe(
        &self,
        cluster: &Arc<ClusterHandle>,
        request: WatchRequest,
    ) -> Result<Subscription> {
        let key = WatchKey {
            cluster: cluster.id.clone(),
            request: request.clone(),
        };

        let entry = match self.entries.get(&key).map(|e| e.value().clone()) {
            Some(existing) => existing,
            None => {
                let created = self.start(cluster, &request).await?;
                // Another task may have started the same watch concurrently;
                // whoever loses cancels their now-redundant watcher.
                match self.entries.entry(key.clone()) {
                    dashmap::Entry::Occupied(occupied) => {
                        created.cancel.cancel();
                        occupied.get().clone()
                    }
                    dashmap::Entry::Vacant(vacant) => {
                        vacant.insert(created.clone());
                        created
                    }
                }
            }
        };

        entry.subscribers.fetch_add(1, Ordering::AcqRel);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscriptions.insert(id, key);

        Ok(Subscription {
            id,
            spec: entry.spec.clone(),
            initial: entry.snapshot(),
            receiver: entry.tx.subscribe(),
        })
    }

    /// Release a subscription. The watch itself lingers for [`IDLE_GRACE`].
    pub fn unsubscribe(&self, id: SubscriptionId) {
        let Some((_, key)) = self.subscriptions.remove(&id) else {
            return;
        };
        let Some(entry) = self.entries.get(&key).map(|e| e.value().clone()) else {
            return;
        };
        if entry.subscribers.fetch_sub(1, Ordering::AcqRel) > 1 {
            return;
        }
        self.schedule_idle_shutdown(key, entry);
    }

    /// Re-read the store, for a subscriber that fell behind the broadcast
    /// buffer and needs to resynchronise.
    pub fn resync(&self, id: SubscriptionId) -> Option<WatchBatch> {
        let key = self.subscriptions.get(&id)?.value().clone();
        self.entries.get(&key).map(|e| e.value().snapshot())
    }

    /// Drop every watch belonging to a cluster (called on disconnect).
    pub fn drop_cluster(&self, cluster: &str) {
        let keys: Vec<WatchKey> = self
            .entries
            .iter()
            .filter(|e| e.key().cluster == cluster)
            .map(|e| e.key().clone())
            .collect();
        for key in keys {
            if let Some((_, entry)) = self.entries.remove(&key) {
                entry.cancel.cancel();
            }
        }
        self.subscriptions.retain(|_, key| key.cluster != cluster);
    }

    pub fn active_watches(&self) -> usize {
        self.entries.len()
    }

    fn schedule_idle_shutdown(&self, key: WatchKey, entry: Arc<WatchEntry>) {
        let entries = self.entries.clone();
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_GRACE).await;
            if entry.subscribers.load(Ordering::Acquire) > 0 {
                return; // someone re-subscribed during the grace period
            }
            entry.cancel.cancel();
            entries.remove(&key);
            tracing::debug!(cluster = %key.cluster, resource = %key.request.resource, "watch retired");
        });
    }

    async fn start(
        &self,
        cluster: &Arc<ClusterHandle>,
        request: &WatchRequest,
    ) -> Result<Arc<WatchEntry>> {
        let discovery = match cluster.discovery() {
            Some(d) => d,
            None => cluster.refresh_discovery().await?,
        };
        let descriptor = discovery.require(&request.resource)?.clone();
        if !descriptor.watchable {
            return Err(CoreError::other(format!(
                "resource `{}` does not support watch",
                descriptor.key
            )));
        }

        let api_resource = descriptor.api_resource();
        let api: Api<DynamicObject> = match (&request.namespace, descriptor.namespaced) {
            (Some(ns), true) => Api::namespaced_with(cluster.client.clone(), ns, &api_resource),
            _ => Api::all_with(cluster.client.clone(), &api_resource),
        };

        let mut config = watcher::Config::default()
            // `any_semantic` lets the apiserver serve the initial list from its
            // cache — much cheaper than a quorum read on large clusters.
            .any_semantic()
            .page_size(500);
        if let Some(sel) = &request.label_selector {
            config = config.labels(sel);
        }
        if let Some(sel) = &request.field_selector {
            config = config.fields(sel);
        }

        let projector = Arc::new(RowProjector::for_resource(&descriptor));
        let spec = projector.spec();
        let writer = reflector::store::Writer::<DynamicObject>::new(api_resource);
        let store = writer.as_reader();
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (state_tx, state_rx) = watch_channel::channel(WatchState::Initializing);
        let epoch = Arc::new(AtomicU64::new(0));
        let cancel = cluster.cancel_token().child_token();

        let entry = Arc::new(WatchEntry {
            store,
            projector: projector.clone(),
            spec,
            tx: tx.clone(),
            state: state_rx,
            epoch: epoch.clone(),
            subscribers: Arc::new(AtomicU64::new(0)),
            cancel: cancel.clone(),
        });

        tokio::spawn(pump(
            writer, api, config, projector, tx, state_tx, epoch, cancel,
        ));

        Ok(entry)
    }
}

/// Drive the watcher stream, maintain the reflector store, and emit batches.
#[allow(clippy::too_many_arguments)]
async fn pump(
    writer: reflector::store::Writer<DynamicObject>,
    api: Api<DynamicObject>,
    config: watcher::Config,
    projector: Arc<RowProjector>,
    tx: broadcast::Sender<Arc<WatchBatch>>,
    state_tx: watch_channel::Sender<WatchState>,
    epoch: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    let stream = reflector(writer, watcher(api, config));
    let mut stream = std::pin::pin!(stream);

    let mut upserts: HashMap<String, Row> = HashMap::new();
    let mut deletes: HashSet<String> = HashSet::new();
    // Objects seen between Init and InitDone; held back so the UI swaps in a
    // complete table rather than watching it fill in row by row.
    let mut init_buffer: Vec<Row> = Vec::new();
    let mut initializing = false;

    let mut ticker = tokio::time::interval(BATCH_WINDOW);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => return,

            event = stream.next() => {
                let Some(event) = event else { return };
                match event {
                    Ok(watcher::Event::Init) => {
                        initializing = true;
                        init_buffer.clear();
                        upserts.clear();
                        deletes.clear();
                        let _ = state_tx.send(WatchState::Initializing);
                    }
                    Ok(watcher::Event::InitApply(obj)) => {
                        init_buffer.push(projector.project(&obj));
                    }
                    Ok(watcher::Event::InitDone) => {
                        initializing = false;
                        let next_epoch = epoch.fetch_add(1, Ordering::AcqRel) + 1;
                        let _ = state_tx.send(WatchState::Ready);
                        let batch = WatchBatch {
                            epoch: next_epoch,
                            snapshot: true,
                            upserts: std::mem::take(&mut init_buffer),
                            deletes: Vec::new(),
                            state: WatchState::Ready,
                        };
                        let _ = tx.send(Arc::new(batch));
                    }
                    Ok(watcher::Event::Apply(obj)) => {
                        let row = projector.project(&obj);
                        deletes.remove(&row.uid);
                        upserts.insert(row.uid.clone(), row);
                    }
                    Ok(watcher::Event::Delete(obj)) => {
                        let row = projector.project(&obj);
                        upserts.remove(&row.uid);
                        deletes.insert(row.uid);
                    }
                    Err(err) => {
                        // `watcher` retries with backoff on its own; surface the
                        // reason without tearing the subscription down.
                        let message = err.to_string();
                        tracing::debug!(%message, "watch error; watcher will retry");
                        let _ = state_tx.send(WatchState::Error { message: message.clone() });
                        let _ = tx.send(Arc::new(WatchBatch {
                            epoch: epoch.load(Ordering::Acquire),
                            snapshot: false,
                            upserts: Vec::new(),
                            deletes: Vec::new(),
                            state: WatchState::Error { message },
                        }));
                    }
                }
            }

            _ = ticker.tick() => {
                if initializing || (upserts.is_empty() && deletes.is_empty()) {
                    continue;
                }
                let batch = WatchBatch {
                    epoch: epoch.load(Ordering::Acquire),
                    snapshot: false,
                    upserts: upserts.drain().map(|(_, row)| row).collect(),
                    deletes: deletes.drain().collect(),
                    state: WatchState::Ready,
                };
                // No subscribers is normal during the idle grace period.
                let _ = tx.send(Arc::new(batch));
            }
        }
    }
}
