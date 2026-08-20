//! Background sampling: keeps node/pod state in reflector stores, polls
//! `metrics.k8s.io`, and keeps an hour of history in memory.
//!
//! Reflectors rather than repeated `list` calls: on a 500-pod cluster a listing
//! every 15 seconds is megabytes of traffic and noticeable apiserver load, and
//! we already need the same objects the resource views watch.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use dashmap::DashMap;
use futures::StreamExt;
use k8s_core::cluster::ClusterHandle;
use k8s_openapi::{
    api::core::v1::{Node, Pod},
    jiff::Timestamp,
};
use kube::{
    Api,
    runtime::{reflector, watcher},
};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    kubelet::{self, PodStats},
    model::{ClusterOverview, NodeScope, Sample},
    objects::{MetricTarget, NamespaceUsage, Point},
    overview::{self, NodeUsage},
    prometheus::PrometheusTarget,
};

/// How often usage is sampled. Matches the metrics-server default resolution;
/// polling faster returns the same numbers.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(15);
/// One hour of history at the sample interval.
const HISTORY_CAPACITY: usize = 4 * 60;
/// Pod-level metrics are only polled while something is asking for them; this
/// is how long a request keeps them warm. Self-cleaning, so a closed panel
/// needs no explicit unsubscribe.
const DEMAND_TTL_MS: i64 = 90_000;

const SCOPES: [NodeScope; 3] = [NodeScope::All, NodeScope::ControlPlane, NodeScope::Workers];

struct History {
    samples: Vec<Sample>,
}

impl History {
    fn push(&mut self, sample: Sample) {
        if self.samples.len() >= HISTORY_CAPACITY {
            self.samples.remove(0);
        }
        self.samples.push(sample);
    }
}

pub struct ClusterSampler {
    nodes: reflector::Store<Node>,
    pods: reflector::Store<Pod>,
    latest: Mutex<HashMap<NodeScope, ClusterOverview>>,
    history: Mutex<HashMap<NodeScope, History>>,
    /// Wall-clock millis of the most recent request for pod-level metrics.
    pod_metrics_wanted: AtomicI64,
    /// Latest per-pod usage, keyed `namespace/pod`.
    pod_usage: Mutex<HashMap<String, (f64, f64)>>,
    /// Usage history per container, keyed `namespace/pod/container`.
    ///
    /// Only collected for workloads someone is looking at: keeping this for
    /// every container in the cluster would be hundreds of series nobody reads.
    container_samples: Mutex<HashMap<String, Vec<UsageSample>>>,
    /// Workloads currently being observed for a recommendation, with the label
    /// selector that finds their pods and when they were last asked for.
    observed: Mutex<HashMap<String, ObservedWorkload>>,
    /// Latest per-node usage from `metrics.k8s.io`.
    node_usage: Mutex<Option<NodeUsage>>,
    /// Latest per-node disk figures from each kubelet.
    node_disk: Mutex<HashMap<String, kubelet::NodeFilesystem>>,
    /// When the node list was last looked at. Disk needs one request per node,
    /// so it is only collected while something is showing it.
    node_disk_wanted: AtomicI64,
    /// Previous kubelet counters per pod, with their timestamp, so network
    /// totals can be differentiated into rates.
    pod_io: Mutex<HashMap<String, (PodStats, i64)>>,
    /// Set when the kubelet summary endpoint is unreachable (usually RBAC).
    io_note: Mutex<Option<String>>,
    /// Latest differentiated I/O rates per pod.
    io_rates: Mutex<HashMap<String, IoRate>>,
    /// Fired when something asks for metrics that are not warm yet, so the
    /// first chart does not wait a whole sampling interval.
    wake: Arc<tokio::sync::Notify>,
    /// Per-namespace snapshot for the heatmap.
    namespace_usage: Mutex<Vec<NamespaceUsage>>,
    /// Rings for targets someone is charting, keyed by `MetricTarget::key`.
    /// Each entry records when it was last asked for so it can be evicted.
    target_history: Mutex<HashMap<String, (i64, Vec<Point>)>>,
    /// Prometheus endpoint, once discovery has run. `Some(None)` means "looked
    /// and found nothing", which is different from "not looked yet".
    prometheus: Mutex<Option<Option<PrometheusTarget>>>,
    cancel: CancellationToken,
}

impl ClusterSampler {
    /// Most recent overview for a scope, if a sample has completed.
    pub fn overview(&self, scope: NodeScope) -> Option<ClusterOverview> {
        self.latest.lock().get(&scope).cloned()
    }

    /// Samples newer than `since_ms` (0 for everything held).
    pub fn history(&self, scope: NodeScope, since_ms: i64) -> Vec<Sample> {
        self.history
            .lock()
            .get(&scope)
            .map(|h| {
                h.samples
                    .iter()
                    .copied()
                    .filter(|s| s.at >= since_ms)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True once the first sample has been published, so the UI can
    /// distinguish "still loading" from "genuinely empty".
    pub fn ready(&self) -> bool {
        !self.latest.lock().is_empty()
    }

    /// Pods currently known, used by the topology builder and object metrics.
    pub fn pods(&self) -> Vec<Pod> {
        self.pods.state().iter().map(|p| (**p).clone()).collect()
    }

    pub fn nodes(&self) -> Vec<Node> {
        self.nodes.state().iter().map(|n| (**n).clone()).collect()
    }

    /// Ask for pod-level metrics. Cheap to call repeatedly; the request expires
    /// on its own so nothing has to remember to switch it off.
    pub fn request_pod_metrics(&self) {
        self.pod_metrics_wanted.store(now_ms(), Ordering::Relaxed);
    }

    fn pod_metrics_wanted(&self) -> bool {
        now_ms() - self.pod_metrics_wanted.load(Ordering::Relaxed) < DEMAND_TTL_MS
    }

    /// Per-node usage, capacity, disk and system information for the node list.
    pub fn node_summaries(&self) -> Vec<crate::objects::NodeSummary> {
        let previous = self.node_disk_wanted.swap(now_ms(), Ordering::Relaxed);
        if now_ms() - previous >= DEMAND_TTL_MS {
            // Cold: collect disk now rather than leaving the column empty for
            // a whole sampling interval.
            self.wake.notify_one();
        }
        let usage = self.node_usage.lock().clone();
        let disk = self.node_disk.lock().clone();
        overview::node_summaries(&self.nodes(), &self.pods(), usage.as_ref(), &disk)
    }

    fn node_disk_wanted(&self) -> bool {
        now_ms() - self.node_disk_wanted.load(Ordering::Relaxed) < DEMAND_TTL_MS
    }

    /// Heatmap rows. Empty until pod metrics have been requested and sampled.
    pub fn namespace_usage(&self) -> Vec<NamespaceUsage> {
        self.namespace_usage.lock().clone()
    }

    /// Start (or refresh) observation of a workload's containers.
    ///
    /// Returns immediately; samples accumulate on the normal tick. Observation
    /// lapses on its own once nobody asks, so nothing has to be unregistered.
    pub fn observe_workload(
        &self,
        key: &str,
        namespace: &str,
        selector: std::collections::BTreeMap<String, String>,
    ) {
        self.request_pod_metrics();
        let mut observed = self.observed.lock();
        let entry = observed.entry(key.to_string()).or_insert(ObservedWorkload {
            last_wanted: now_ms(),
            namespace: namespace.to_string(),
            selector,
            pods: Vec::new(),
        });
        entry.last_wanted = now_ms();
    }

    /// Samples collected for one container of an observed workload.
    ///
    /// Pods are treated as independent observations of the same container,
    /// which is what sizing a single replica actually needs.
    pub fn container_history(&self, key: &str, container: &str) -> Vec<UsageSample> {
        let pods = self
            .observed
            .lock()
            .get(key)
            .map(|entry| entry.pods.clone())
            .unwrap_or_default();

        let samples = self.container_samples.lock();
        let mut out: Vec<UsageSample> = pods
            .iter()
            .flat_map(|pod| {
                samples
                    .get(&format!("{pod}/{container}"))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        out.sort_by_key(|(at, _, _)| *at);
        out
    }

    /// Latest usage for one pod, if known.
    pub fn pod_usage(&self, namespace: &str, pod: &str) -> Option<(f64, f64)> {
        self.pod_usage
            .lock()
            .get(&format!("{namespace}/{pod}"))
            .copied()
    }

    /// History accumulated for a charted target, marking it as still wanted.
    pub fn target_history(&self, target: &MetricTarget) -> Vec<Point> {
        let key = target.key();
        let points = {
            let mut history = self.target_history.lock();
            let entry = history.entry(key).or_insert_with(|| (now_ms(), Vec::new()));
            entry.0 = now_ms();
            entry.1.clone()
        };
        // Registering the target is what makes the next tick collect it, so a
        // brand-new chart also needs the sampler woken.
        self.request_pod_metrics();
        if points.is_empty() {
            self.wake.notify_one();
        }
        points
    }

    /// Explanation for missing network/disk series, if any.
    pub fn io_note(&self) -> Option<String> {
        self.io_note.lock().clone()
    }

    /// Cached Prometheus target; `None` when the cluster has none.
    pub fn prometheus(&self) -> Option<PrometheusTarget> {
        self.prometheus.lock().clone().flatten()
    }

    pub fn prometheus_checked(&self) -> bool {
        self.prometheus.lock().is_some()
    }

    fn stop(&self) {
        self.cancel.cancel();
    }
}

fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

impl Drop for ClusterSampler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One sampler per connected cluster.
#[derive(Default)]
pub struct MetricsManager {
    samplers: DashMap<String, Arc<ClusterSampler>>,
}

impl MetricsManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start sampling a cluster, or return the existing sampler.
    pub fn ensure(&self, cluster: &Arc<ClusterHandle>) -> Arc<ClusterSampler> {
        if let Some(existing) = self.samplers.get(&cluster.id) {
            return existing.clone();
        }
        let sampler = start(cluster);
        match self.samplers.entry(cluster.id.clone()) {
            dashmap::Entry::Occupied(occupied) => {
                sampler.stop();
                occupied.get().clone()
            }
            dashmap::Entry::Vacant(vacant) => {
                vacant.insert(sampler.clone());
                sampler
            }
        }
    }

    pub fn drop_cluster(&self, cluster: &str) {
        if let Some((_, sampler)) = self.samplers.remove(cluster) {
            sampler.stop();
        }
    }
}

fn start(cluster: &Arc<ClusterHandle>) -> Arc<ClusterSampler> {
    let cancel = cluster.cancel_token().child_token();

    let node_writer = reflector::store::Writer::<Node>::default();
    let nodes = node_writer.as_reader();
    let pod_writer = reflector::store::Writer::<Pod>::default();
    let pods = pod_writer.as_reader();

    spawn_reflector(
        node_writer,
        Api::<Node>::all(cluster.client.clone()),
        cancel.clone(),
        "nodes",
    );
    spawn_reflector(
        pod_writer,
        Api::<Pod>::all(cluster.client.clone()),
        cancel.clone(),
        "pods",
    );

    let sampler = Arc::new(ClusterSampler {
        nodes,
        pods,
        latest: Mutex::new(HashMap::new()),
        history: Mutex::new(HashMap::new()),
        pod_metrics_wanted: AtomicI64::new(0),
        pod_usage: Mutex::new(HashMap::new()),
        container_samples: Mutex::new(HashMap::new()),
        observed: Mutex::new(HashMap::new()),
        node_usage: Mutex::new(None),
        node_disk: Mutex::new(HashMap::new()),
        node_disk_wanted: AtomicI64::new(0),
        pod_io: Mutex::new(HashMap::new()),
        io_note: Mutex::new(None),
        io_rates: Mutex::new(HashMap::new()),
        wake: Arc::new(tokio::sync::Notify::new()),
        namespace_usage: Mutex::new(Vec::new()),
        target_history: Mutex::new(HashMap::new()),
        prometheus: Mutex::new(None),
        cancel: cancel.clone(),
    });

    spawn_sampling(cluster.clone(), sampler.clone(), cancel);
    sampler
}

fn spawn_reflector<K>(
    writer: reflector::store::Writer<K>,
    api: Api<K>,
    cancel: CancellationToken,
    what: &'static str,
) where
    K: kube::Resource<DynamicType = ()>
        + Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + Send
        + Sync
        + 'static,
{
    tokio::spawn(async move {
        let stream = reflector(
            writer,
            watcher(api, watcher::Config::default().any_semantic()),
        );
        let mut stream = std::pin::pin!(stream);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                event = stream.next() => match event {
                    Some(Ok(_)) => {}
                    Some(Err(err)) => tracing::debug!(%err, what, "metrics reflector error; retrying"),
                    None => return,
                },
            }
        }
    });
}

fn spawn_sampling(
    cluster: Arc<ClusterHandle>,
    sampler: Arc<ClusterSampler>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        // Wait for both initial listings before the first sample. Sampling on a
        // timer alone publishes an overview built from a half-filled store —
        // on a 500-pod cluster that means a confident "0 pods, 0 requests"
        // sitting on screen until the next tick.
        let warmup = tokio::time::timeout(Duration::from_secs(60), async {
            let _ = sampler.nodes.wait_until_ready().await;
            let _ = sampler.pods.wait_until_ready().await;
        });
        if warmup.await.is_err() {
            tracing::warn!("initial node/pod listing did not complete in 60s; sampling anyway");
        }

        // Discovery runs once per connection: the answer only changes when
        // someone installs or removes Prometheus.
        let found = crate::prometheus::discover(&cluster).await;
        *sampler.prometheus.lock() = Some(found);

        let mut ticker = tokio::time::interval(SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            sample_once(&cluster, &sampler).await;
            let wake = sampler.wake.clone();
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = ticker.tick() => {}
                // A newly opened chart asks for an immediate sample rather than
                // waiting out the interval.
                _ = wake.notified() => {}
            }
        }
    });
}

async fn sample_once(cluster: &Arc<ClusterHandle>, sampler: &Arc<ClusterSampler>) {
    let nodes: Vec<Node> = sampler
        .nodes
        .state()
        .iter()
        .map(|n| (**n).clone())
        .collect();
    let pods: Vec<Pod> = sampler.pods.state().iter().map(|p| (**p).clone()).collect();

    let (usage, error) = match node_usage(cluster).await {
        Ok(usage) => (Some(usage), None),
        Err(err) => (None, Some(err)),
    };
    *sampler.node_usage.lock() = usage.clone();

    let now = Timestamp::now();
    let stamp = now.to_string();

    if sampler.pod_metrics_wanted() {
        sample_pod_metrics(cluster, sampler, &pods).await;
    }
    if sampler.node_disk_wanted() {
        sample_node_disk(cluster, sampler, &nodes).await;
    }

    for scope in SCOPES {
        let overview = overview::build(
            &nodes,
            &pods,
            usage.as_ref(),
            error.clone(),
            scope,
            stamp.clone(),
        );
        let sample = Sample::new(now, &overview);
        sampler.latest.lock().insert(scope, overview);
        sampler
            .history
            .lock()
            .entry(scope)
            .or_insert_with(|| History {
                samples: Vec::new(),
            })
            .push(sample);
    }
}

/// Read per-node usage from the metrics API.
///
/// Returns a message rather than an error type: a missing metrics-server is a
/// normal cluster configuration, not a failure of this app, and the UI says so.
async fn node_usage(cluster: &Arc<ClusterHandle>) -> Result<NodeUsage, String> {
    let request = http::Request::builder()
        .uri("/apis/metrics.k8s.io/v1beta1/nodes")
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|err| err.to_string())?;

    let response: Value =
        cluster
            .client
            .request::<Value>(request)
            .await
            .map_err(|err| match err {
                kube::Error::Api(status) if status.code == 404 => {
                    "metrics-server is not installed in this cluster".to_string()
                }
                kube::Error::Api(status) if status.code == 403 => {
                    "no permission to read metrics.k8s.io".to_string()
                }
                other => other.to_string(),
            })?;

    let mut usage = NodeUsage::default();
    for item in response
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = item.pointer("/metadata/name").and_then(Value::as_str) else {
            continue;
        };
        if let Some(cpu) = item
            .pointer("/usage/cpu")
            .and_then(Value::as_str)
            .and_then(crate::quantity::parse)
        {
            usage.cpu.insert(name.to_string(), cpu);
        }
        if let Some(memory) = item
            .pointer("/usage/memory")
            .and_then(Value::as_str)
            .and_then(crate::quantity::parse)
        {
            usage.memory.insert(name.to_string(), memory);
        }
    }
    Ok(usage)
}

/// Poll per-pod usage and fold it into the namespace heatmap and any charted
/// targets. Only runs while something is asking for it.
async fn sample_pod_metrics(
    cluster: &Arc<ClusterHandle>,
    sampler: &Arc<ClusterSampler>,
    pods: &[Pod],
) {
    let (usage, per_container) = match pod_usage(cluster).await {
        Ok(usage) => usage,
        Err(err) => {
            tracing::debug!(%err, "pod metrics unavailable");
            return;
        }
    };

    *sampler.pod_usage.lock() = usage.clone();
    record_observations(sampler, pods, &per_container);
    *sampler.namespace_usage.lock() = overview::namespace_usage(pods, &usage);

    sample_pod_io(cluster, sampler, pods).await;

    // Append a point to every target that is still being charted, and drop the
    // rings nobody has looked at recently.
    let now = now_ms();
    let mut history = sampler.target_history.lock();
    history.retain(|_, (last_wanted, _)| now - *last_wanted < DEMAND_TTL_MS);

    let io = sampler.io_rates.lock().clone();
    let keys: Vec<String> = history.keys().cloned().collect();
    for key in keys {
        let Some(point) = point_for_key(&key, pods, &usage, &io, now) else {
            continue;
        };
        if let Some((_, points)) = history.get_mut(&key) {
            if points.len() >= HISTORY_CAPACITY {
                points.remove(0);
            }
            points.push(point);
        }
    }
}

/// Compute the current value for a ring key without re-parsing the target.
fn point_for_key(
    key: &str,
    pods: &[Pod],
    usage: &HashMap<String, (f64, f64)>,
    io: &HashMap<String, IoRate>,
    now: i64,
) -> Option<Point> {
    let mut parts = key.split('/');
    let kind = parts.next()?;
    let mut io_rate = IoRate::default();

    let (cpu, memory) = match kind {
        "pod" => {
            let namespace = parts.next()?;
            let name = parts.next()?;
            let id = format!("{namespace}/{name}");
            if let Some(rate) = io.get(&id) {
                io_rate = *rate;
            }
            usage.get(&id).copied()?
        }
        "ns" => {
            let namespace = parts.next()?;
            sum_usage(usage, |ns, _| ns == namespace)
        }
        "node" => {
            let node = parts.next()?;
            let on_node: std::collections::HashSet<String> = pods
                .iter()
                .filter(|pod| pod.spec.as_ref().and_then(|s| s.node_name.as_deref()) == Some(node))
                .map(|pod| {
                    format!(
                        "{}/{}",
                        pod.metadata.namespace.clone().unwrap_or_default(),
                        pod.metadata.name.clone().unwrap_or_default()
                    )
                })
                .collect();
            for (id, rate) in io {
                if on_node.contains(id) {
                    io_rate.rx += rate.rx;
                    io_rate.tx += rate.tx;
                    io_rate.fs_used += rate.fs_used;
                    io_rate.volume_used += rate.volume_used;
                }
            }
            usage
                .iter()
                .filter(|(id, _)| on_node.contains(*id))
                .fold((0.0, 0.0), |acc, (_, (cpu, memory))| {
                    (acc.0 + cpu, acc.1 + memory)
                })
        }
        // Workload rings are filled by the command layer, which knows the
        // selector; nothing to do on the tick.
        _ => return None,
    };

    Some(Point {
        at: now,
        cpu,
        memory,
        network_rx: io_rate.rx,
        network_tx: io_rate.tx,
        fs_used: io_rate.fs_used,
        volume_used: io_rate.volume_used,
    })
}

/// Differentiated network rates plus current disk usage for one pod.
#[derive(Debug, Clone, Copy, Default)]
pub struct IoRate {
    pub rx: f64,
    pub tx: f64,
    pub fs_used: f64,
    pub volume_used: f64,
}

/// Fetch kubelet summaries for the nodes hosting charted targets only.
///
/// Querying every node would be one request per node per tick; on a large
/// cluster that is a lot of traffic for data nobody is looking at.
async fn sample_pod_io(cluster: &Arc<ClusterHandle>, sampler: &Arc<ClusterSampler>, pods: &[Pod]) {
    let keys: Vec<String> = sampler.target_history.lock().keys().cloned().collect();
    if keys.is_empty() {
        return;
    }

    let mut nodes: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in &keys {
        let mut parts = key.split('/');
        match parts.next() {
            Some("node") => {
                if let Some(node) = parts.next() {
                    nodes.insert(node.to_string());
                }
            }
            Some("pod") => {
                let (Some(namespace), Some(name)) = (parts.next(), parts.next()) else {
                    continue;
                };
                if let Some(node) = pods
                    .iter()
                    .find(|pod| {
                        pod.metadata.namespace.as_deref() == Some(namespace)
                            && pod.metadata.name.as_deref() == Some(name)
                    })
                    .and_then(|pod| pod.spec.as_ref().and_then(|s| s.node_name.clone()))
                {
                    nodes.insert(node);
                }
            }
            _ => {}
        }
    }

    let now = now_ms();
    let mut fresh: HashMap<String, PodStats> = HashMap::new();
    let mut note: Option<String> = None;

    for node in nodes {
        match kubelet::node_summary(cluster, &node).await {
            Ok(stats) => fresh.extend(stats),
            Err(err) => {
                tracing::debug!(node = %node, %err, "kubelet summary unavailable");
                note = Some(err);
            }
        }
    }
    *sampler.io_note.lock() = note;

    let mut previous = sampler.pod_io.lock();
    let mut rates: HashMap<String, IoRate> = HashMap::new();
    for (id, current) in &fresh {
        if let Some((old, at)) = previous.get(id) {
            let seconds = (now - at) as f64 / 1000.0;
            rates.insert(
                id.clone(),
                IoRate {
                    rx: kubelet::rate(old.rx_bytes, current.rx_bytes, seconds),
                    tx: kubelet::rate(old.tx_bytes, current.tx_bytes, seconds),
                    fs_used: current.fs_used_bytes,
                    volume_used: current.volume_used_bytes,
                },
            );
        } else {
            // First observation has no interval to differentiate over; report
            // the absolute disk figures and leave the rates at zero.
            rates.insert(
                id.clone(),
                IoRate {
                    rx: 0.0,
                    tx: 0.0,
                    fs_used: current.fs_used_bytes,
                    volume_used: current.volume_used_bytes,
                },
            );
        }
        previous.insert(id.clone(), (*current, now));
    }
    drop(previous);

    *sampler.io_rates.lock() = rates;
}

fn sum_usage(
    usage: &HashMap<String, (f64, f64)>,
    predicate: impl Fn(&str, &str) -> bool,
) -> (f64, f64) {
    usage
        .iter()
        .filter(|(id, _)| {
            let mut parts = id.splitn(2, '/');
            match (parts.next(), parts.next()) {
                (Some(ns), Some(name)) => predicate(ns, name),
                _ => false,
            }
        })
        .fold((0.0, 0.0), |acc, (_, (cpu, memory))| {
            (acc.0 + cpu, acc.1 + memory)
        })
}

/// One usage observation: when, how many cores, how many bytes.
type UsageSample = (i64, f64, f64);

/// A workload whose containers are being sampled for a recommendation.
struct ObservedWorkload {
    last_wanted: i64,
    namespace: String,
    selector: std::collections::BTreeMap<String, String>,
    /// Pods matched on the most recent tick, keyed `namespace/pod`.
    pods: Vec<String>,
}

/// Samples kept per container. At 15s a tick this is six hours.
const CONTAINER_HISTORY: usize = 4 * 60 * 6;

/// Record per-container usage for the workloads being observed.
fn record_observations(
    sampler: &Arc<ClusterSampler>,
    pods: &[Pod],
    per_container: &HashMap<String, (f64, f64)>,
) {
    use kube::ResourceExt;

    let now = now_ms();
    let mut observed = sampler.observed.lock();
    observed.retain(|_, entry| now - entry.last_wanted < DEMAND_TTL_MS);
    if observed.is_empty() {
        // Nothing is being observed, so the history is dead weight.
        sampler.container_samples.lock().clear();
        return;
    }

    let mut wanted: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in observed.values_mut() {
        entry.pods = pods
            .iter()
            .filter(|pod| {
                pod.namespace().as_deref() == Some(entry.namespace.as_str())
                    && !entry.selector.is_empty()
                    && entry
                        .selector
                        .iter()
                        .all(|(key, value)| pod.labels().get(key) == Some(value))
            })
            .map(|pod| format!("{}/{}", entry.namespace, pod.name_any()))
            .collect();
        wanted.extend(entry.pods.iter().cloned());
    }

    let mut samples = sampler.container_samples.lock();
    // Drop history for pods no longer part of any observed workload — a
    // rollout would otherwise accumulate every replaced replica forever.
    samples.retain(|key, _| {
        key.rsplit_once('/')
            .is_some_and(|(pod, _)| wanted.contains(pod))
    });

    for (key, (cpu, memory)) in per_container {
        let Some((pod, _)) = key.rsplit_once('/') else {
            continue;
        };
        if !wanted.contains(pod) {
            continue;
        }
        let series = samples.entry(key.clone()).or_default();
        if series.len() >= CONTAINER_HISTORY {
            series.remove(0);
        }
        series.push((now, *cpu, *memory));
    }
}

/// Per-pod and per-container usage from `metrics.k8s.io`.
///
/// Returns `(namespace/pod, …)` and `(namespace/pod/container, …)`; the second
/// is what sizing a container needs, the first is what everything else uses.
#[allow(clippy::type_complexity)]
async fn pod_usage(
    cluster: &Arc<ClusterHandle>,
) -> Result<(HashMap<String, (f64, f64)>, HashMap<String, (f64, f64)>), String> {
    let request = http::Request::builder()
        .uri("/apis/metrics.k8s.io/v1beta1/pods")
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|err| err.to_string())?;

    let response: Value = cluster
        .client
        .request::<Value>(request)
        .await
        .map_err(|err| err.to_string())?;

    let mut out = HashMap::new();
    let mut per_container = HashMap::new();

    for item in response
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(namespace), Some(name)) = (
            item.pointer("/metadata/namespace").and_then(Value::as_str),
            item.pointer("/metadata/name").and_then(Value::as_str),
        ) else {
            continue;
        };

        // A pod's usage is the sum over its containers; the API reports each
        // container separately, and sizing needs them separately too.
        let mut cpu = 0.0;
        let mut memory = 0.0;
        for container in item
            .get("containers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let container_cpu = container
                .pointer("/usage/cpu")
                .and_then(Value::as_str)
                .and_then(crate::quantity::parse)
                .unwrap_or(0.0);
            let container_memory = container
                .pointer("/usage/memory")
                .and_then(Value::as_str)
                .and_then(crate::quantity::parse)
                .unwrap_or(0.0);

            cpu += container_cpu;
            memory += container_memory;

            if let Some(container_name) = container.get("name").and_then(Value::as_str) {
                per_container.insert(
                    format!("{namespace}/{name}/{container_name}"),
                    (container_cpu, container_memory),
                );
            }
        }
        out.insert(format!("{namespace}/{name}"), (cpu, memory));
    }
    Ok((out, per_container))
}

/// Collect disk usage for every node, one kubelet request each.
///
/// Only runs while the node list is on screen; on a large cluster this is the
/// most expensive thing in the sampler, and nobody is reading it otherwise.
async fn sample_node_disk(
    cluster: &Arc<ClusterHandle>,
    sampler: &Arc<ClusterSampler>,
    nodes: &[Node],
) {
    use kube::ResourceExt;

    let requests = nodes.iter().map(|node| {
        let name = node.name_any();
        let cluster = cluster.clone();
        async move {
            let result = kubelet::node_filesystem(&cluster, &name).await;
            (name, result)
        }
    });

    let mut fresh = HashMap::new();
    for (name, result) in futures::future::join_all(requests).await {
        match result {
            Ok(filesystem) => {
                fresh.insert(name, filesystem);
            }
            Err(err) => {
                tracing::debug!(node = %name, %err, "node disk stats unavailable");
                *sampler.io_note.lock() = Some(err);
            }
        }
    }
    *sampler.node_disk.lock() = fresh;
}
