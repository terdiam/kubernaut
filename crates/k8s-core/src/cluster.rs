//! Multi-cluster connection manager.
//!
//! One [`ClusterHandle`] per kubeconfig context. Connections are lazy: opening
//! the app must not fan out to every cluster in the kubeconfig, because a
//! single unreachable VPN-only cluster would otherwise stall startup.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use http::{HeaderName, HeaderValue};
use kube::{Client, Config};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    discovery::DiscoveryCache,
    error::{CoreError, Result},
    kubeconfig::{self, ContextEntry, LoadedKubeconfig},
};

/// Kubeconfig context name. Unique per app session and stable across restarts.
pub type ClusterId = String;

const PROBE_INTERVAL: Duration = Duration::from_secs(10);
const PROBE_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Live connection state, streamed to the UI as a status badge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ClusterStatus {
    Connecting,
    /// Reachable; `version` is the apiserver git version.
    Connected {
        version: String,
    },
    /// Reachable but a probe failed at least once — usually a flaky VPN.
    Degraded {
        reason: String,
    },
    Unreachable {
        reason: String,
    },
    Disconnected,
}

/// Per-connection overrides the user can set in the cluster dialog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOptions {
    /// `kubectl --as`
    pub impersonate_user: Option<String>,
    /// `kubectl --as-group` (repeatable)
    #[serde(default)]
    pub impersonate_groups: Vec<String>,
    /// Override the context's default namespace.
    pub default_namespace: Option<String>,
    /// `kubectl --insecure-skip-tls-verify`. Off by default and surfaced in the
    /// UI as a warning, since it disables server identity checks entirely.
    #[serde(default)]
    pub accept_invalid_certs: bool,
    /// SOCKS5/HTTP proxy for this cluster only.
    pub proxy_url: Option<String>,
}

/// An open connection to one cluster.
pub struct ClusterHandle {
    pub id: ClusterId,
    pub client: Client,
    /// Kept so subresource clients (exec, port-forward) can be rebuilt without
    /// re-reading the kubeconfig.
    pub config: Config,
    pub default_namespace: String,
    pub options: ConnectOptions,
    discovery: ArcSwapOption<DiscoveryCache>,
    status_tx: watch::Sender<ClusterStatus>,
    cancel: CancellationToken,
}

impl ClusterHandle {
    pub fn status(&self) -> ClusterStatus {
        self.status_tx.borrow().clone()
    }

    pub fn watch_status(&self) -> watch::Receiver<ClusterStatus> {
        self.status_tx.subscribe()
    }

    /// Last successful discovery, if any. `None` until the first run finishes.
    pub fn discovery(&self) -> Option<Arc<DiscoveryCache>> {
        self.discovery.load_full()
    }

    /// Run discovery and publish the result. Safe to call repeatedly; callers
    /// use it to pick up newly installed CRDs.
    pub async fn refresh_discovery(&self) -> Result<Arc<DiscoveryCache>> {
        let cache = Arc::new(DiscoveryCache::run(&self.id, self.client.clone()).await?);
        self.discovery.store(Some(cache.clone()));
        Ok(cache)
    }

    /// Stop the health prober and any tasks scoped to this connection.
    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.status_tx.send_replace(ClusterStatus::Disconnected);
    }

    /// Token that fires when this cluster is disconnected — watch tasks use it
    /// so they never outlive the connection they read from.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl Drop for ClusterHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Owns every open cluster connection plus the parsed kubeconfig.
pub struct ClusterManager {
    clusters: DashMap<ClusterId, Arc<ClusterHandle>>,
    kubeconfig: RwLock<Arc<LoadedKubeconfig>>,
    /// Kubeconfig files this app manages, merged on top of the system one.
    ///
    /// Kept separate on purpose: importing a cluster must never rewrite
    /// `~/.kube/config`. That file is shared with kubectl and every other tool,
    /// and a UI that edits it can break workflows it knows nothing about.
    managed: RwLock<Vec<std::path::PathBuf>>,
}

impl ClusterManager {
    pub fn new(loaded: LoadedKubeconfig) -> Self {
        Self {
            clusters: DashMap::new(),
            kubeconfig: RwLock::new(Arc::new(loaded)),
            managed: RwLock::new(Vec::new()),
        }
    }

    /// Load the kubeconfig from disk and build an empty manager.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(kubeconfig::load()?))
    }

    /// Load only the kubeconfigs this app manages.
    ///
    /// The system kubeconfig is not read: clusters are added explicitly, so
    /// nothing is reachable merely because a file exists on disk.
    pub fn from_managed(managed: Vec<std::path::PathBuf>) -> Result<Self> {
        let loaded = kubeconfig::load_files(&managed)?;
        let manager = Self::new(loaded);
        *manager.managed.write() = managed;
        Ok(manager)
    }

    /// Replace the managed file list and reload.
    pub fn set_managed(&self, managed: Vec<std::path::PathBuf>) -> Result<Vec<ContextEntry>> {
        *self.managed.write() = managed;
        self.reload_kubeconfig()
    }

    /// Re-read kubeconfig files. Open connections are left untouched, so a
    /// reload never drops a session the user is working in.
    pub fn reload_kubeconfig(&self) -> Result<Vec<ContextEntry>> {
        let managed = self.managed.read().clone();
        let loaded = Arc::new(kubeconfig::load_files(&managed)?);
        let entries = kubeconfig::contexts(&loaded);
        *self.kubeconfig.write() = loaded;
        Ok(entries)
    }

    /// Single-context kubeconfig YAML for an embedded shell. See
    /// [`kubeconfig::minified`] for why it is not the whole file.
    pub fn minified_kubeconfig(&self, context: &str, namespace: Option<&str>) -> Result<String> {
        let loaded = self.kubeconfig.read().clone();
        kubeconfig::minified(&loaded, context, namespace)
    }

    pub fn contexts(&self) -> Vec<ContextEntry> {
        let loaded = self.kubeconfig.read().clone();
        kubeconfig::contexts(&loaded)
    }

    pub fn get(&self, id: &str) -> Option<Arc<ClusterHandle>> {
        self.clusters.get(id).map(|e| e.value().clone())
    }

    pub fn require(&self, id: &str) -> Result<Arc<ClusterHandle>> {
        self.get(id)
            .ok_or_else(|| CoreError::UnknownCluster(id.to_string()))
    }

    pub fn connected(&self) -> Vec<ClusterId> {
        self.clusters.iter().map(|e| e.key().clone()).collect()
    }

    /// Connect to a context, or return the existing handle if already open.
    pub async fn connect(
        &self,
        context: &str,
        options: ConnectOptions,
    ) -> Result<Arc<ClusterHandle>> {
        if let Some(existing) = self.get(context) {
            return Ok(existing);
        }

        let loaded = self.kubeconfig.read().clone();
        if !loaded.config.contexts.iter().any(|c| c.name == context) {
            return Err(CoreError::UnknownContext(context.to_string()));
        }

        let mut config = Config::from_custom_kubeconfig(
            loaded.config.clone(),
            &kubeconfig::options_for(context),
        )
        .await?;

        apply_options(&mut config, &options)?;

        let default_namespace = options
            .default_namespace
            .clone()
            .unwrap_or_else(|| config.default_namespace.clone());

        let client = Client::try_from(config.clone())
            .map_err(|source| CoreError::client_build(context, source))?;

        // The receiver is dropped immediately; subscribers attach later via
        // `watch_status`. Every write below therefore uses `send_replace`,
        // which stores the value whether or not anyone is listening —
        // `send` fails outright with no receivers, which left the status
        // stuck on `Connecting` after a successful connection.
        let (status_tx, _) = watch::channel(ClusterStatus::Connecting);
        let handle = Arc::new(ClusterHandle {
            id: context.to_string(),
            client,
            config,
            default_namespace,
            options,
            discovery: ArcSwapOption::empty(),
            status_tx,
            cancel: CancellationToken::new(),
        });

        // First probe is inline so `connect` fails fast on a bad kubeconfig
        // instead of handing back a handle that will never work.
        let version = handle
            .client
            .apiserver_version()
            .await
            .map_err(|source| CoreError::client_build(context, source))?;
        handle.status_tx.send_replace(ClusterStatus::Connected {
            version: version.git_version.clone(),
        });

        spawn_prober(handle.clone());

        self.clusters.insert(context.to_string(), handle.clone());
        Ok(handle)
    }

    /// Close a connection and drop its cached discovery.
    pub fn disconnect(&self, id: &str) {
        if let Some((_, handle)) = self.clusters.remove(id) {
            handle.shutdown();
        }
    }

    pub fn disconnect_all(&self) {
        let ids: Vec<_> = self.clusters.iter().map(|e| e.key().clone()).collect();
        for id in ids {
            self.disconnect(&id);
        }
    }
}

fn apply_options(config: &mut Config, options: &ConnectOptions) -> Result<()> {
    if let Some(ns) = &options.default_namespace {
        config.default_namespace = ns.clone();
    }
    if options.accept_invalid_certs {
        config.accept_invalid_certs = true;
    }
    if let Some(proxy) = &options.proxy_url {
        config.proxy_url = Some(
            proxy
                .parse()
                .map_err(|e| CoreError::other(format!("invalid proxy url `{proxy}`: {e}")))?,
        );
    }
    if let Some(user) = &options.impersonate_user {
        config.headers.push((
            HeaderName::from_static("impersonate-user"),
            HeaderValue::from_str(user)
                .map_err(|e| CoreError::other(format!("invalid impersonation user: {e}")))?,
        ));
    }
    for group in &options.impersonate_groups {
        config.headers.push((
            HeaderName::from_static("impersonate-group"),
            HeaderValue::from_str(group)
                .map_err(|e| CoreError::other(format!("invalid impersonation group: {e}")))?,
        ));
    }
    Ok(())
}

/// Poll the apiserver so the UI can distinguish "cluster is slow" from
/// "the app stopped updating". Backs off on failure to avoid hammering an
/// apiserver that is already struggling.
fn spawn_prober(handle: Arc<ClusterHandle>) {
    let cancel = handle.cancel_token();
    tokio::spawn(async move {
        let mut backoff = PROBE_INTERVAL;
        let mut consecutive_failures = 0u32;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(backoff) => {}
            }

            let started = Instant::now();
            match handle.client.apiserver_version().await {
                Ok(info) => {
                    consecutive_failures = 0;
                    backoff = PROBE_INTERVAL;
                    handle.status_tx.send_replace(ClusterStatus::Connected {
                        version: info.git_version,
                    });
                    tracing::trace!(cluster = %handle.id, elapsed = ?started.elapsed(), "probe ok");
                }
                Err(err) => {
                    consecutive_failures += 1;
                    let reason = err.to_string();
                    // One miss is a blip; three in a row means the cluster is
                    // genuinely gone.
                    let status = if consecutive_failures < 3 {
                        ClusterStatus::Degraded { reason }
                    } else {
                        ClusterStatus::Unreachable { reason }
                    };
                    handle.status_tx.send_replace(status);
                    backoff = (backoff * 2).min(PROBE_BACKOFF_MAX);
                    tracing::debug!(cluster = %handle.id, failures = consecutive_failures, "probe failed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `watch::Sender::send` fails when no receiver exists, and the status
    /// channel starts with none — subscribers attach after `connect` returns.
    /// Using `send` there left every freshly connected cluster reporting
    /// `Connecting` until the health prober's first tick ten seconds later.
    #[tokio::test]
    async fn status_updates_survive_having_no_subscribers() {
        let (sender, receiver) = watch::channel(ClusterStatus::Connecting);
        drop(receiver);

        assert!(
            sender
                .send(ClusterStatus::Connected {
                    version: "v1.33".into()
                })
                .is_err(),
            "this is the trap: `send` reports failure with no receivers"
        );

        sender.send_replace(ClusterStatus::Connected {
            version: "v1.33".into(),
        });

        // A subscriber attaching afterwards sees the stored value.
        let late = sender.subscribe();
        assert_eq!(
            *late.borrow(),
            ClusterStatus::Connected {
                version: "v1.33".into()
            }
        );
    }
}
