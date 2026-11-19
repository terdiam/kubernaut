//! What the `*_ops` modules need of the app, named one piece at a time.
//!
//! [`AppCore`] holds everything, and a function that takes it says nothing
//! about which part it touches. Each trait here is one part. `AppCore` and a
//! shared `AppCore` implement them, so no caller changes, and a test can stand
//! in for the app with only the part under test.

use std::{fmt::Display, sync::Arc};

use k8s_core::cluster::ClusterHandle;
use k8s_helm::Helm;
use k8s_ops::kustomize::Kustomize;

use crate::state::AppCore;

/// Finds a connected cluster by its id.
pub trait ClusterSource {
    /// The cluster, or why it cannot be had.
    fn handle(&self, cluster: &str) -> Result<Arc<ClusterHandle>, String>;
}

impl ClusterSource for AppCore {
    fn handle(&self, cluster: &str) -> Result<Arc<ClusterHandle>, String> {
        self.clusters.require(cluster).map_err(|e| e.to_string())
    }
}

/// A shared `AppCore` is passed as it is held, so it is a source too.
impl<T: ClusterSource + ?Sized> ClusterSource for Arc<T> {
    fn handle(&self, cluster: &str) -> Result<Arc<ClusterHandle>, String> {
        (**self).handle(cluster)
    }
}

/// Writes an action and its outcome to the audit log.
pub trait Audited {
    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    );
}

impl Audited for AppCore {
    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        self.record_audit(cluster, action, target, result);
    }
}

impl<H: Audited + ?Sized> Audited for Arc<H> {
    fn audit<T, E: Display>(
        &self,
        cluster: &str,
        action: &str,
        target: &str,
        result: &Result<T, E>,
    ) {
        (**self).audit(cluster, action, target, result);
    }
}

/// An app that may change a cluster, and says so in the audit log when it does.
///
/// Whether a change is allowed is the app's to say: a cluster marked protected
/// in Settings refuses every one, however confirmed.
pub trait WriteHost: ClusterSource + Audited {
    /// `Err` with the reason when changes to `cluster` are refused.
    fn ensure_writable(&self, cluster: &str) -> Result<(), String>;
}

impl WriteHost for AppCore {
    fn ensure_writable(&self, cluster: &str) -> Result<(), String> {
        AppCore::ensure_writable(self, cluster)
    }
}

impl<H: WriteHost + ?Sized> WriteHost for Arc<H> {
    fn ensure_writable(&self, cluster: &str) -> Result<(), String> {
        (**self).ensure_writable(cluster)
    }
}

/// The helm binary, resolved on first use. It may be absent, which must not stop
/// the app from listing releases, so asking for it can fail.
pub trait HelmSource {
    fn helm(&self) -> k8s_helm::Result<Arc<Helm>>;
}

impl HelmSource for AppCore {
    fn helm(&self) -> k8s_helm::Result<Arc<Helm>> {
        AppCore::helm(self)
    }
}

impl<H: HelmSource + ?Sized> HelmSource for Arc<H> {
    fn helm(&self) -> k8s_helm::Result<Arc<Helm>> {
        (**self).helm()
    }
}

/// The kustomize binary, resolved on first use; asking for it can fail.
pub trait KustomizeSource {
    fn kustomize(&self) -> k8s_ops::error::Result<Arc<Kustomize>>;
}

impl KustomizeSource for AppCore {
    fn kustomize(&self) -> k8s_ops::error::Result<Arc<Kustomize>> {
        AppCore::kustomize(self)
    }
}

impl<H: KustomizeSource + ?Sized> KustomizeSource for Arc<H> {
    fn kustomize(&self) -> k8s_ops::error::Result<Arc<Kustomize>> {
        (**self).kustomize()
    }
}

/// A kubeconfig that reaches one cluster and nothing else, for a command run on
/// the cluster's behalf, such as helm.
pub trait KubeconfigSource: ClusterSource {
    /// The kubeconfig text for `cluster`, with `namespace` pinned when given.
    fn minified_kubeconfig(&self, cluster: &str, namespace: Option<&str>)
    -> Result<String, String>;
}

impl KubeconfigSource for AppCore {
    fn minified_kubeconfig(
        &self,
        cluster: &str,
        namespace: Option<&str>,
    ) -> Result<String, String> {
        self.clusters
            .minified_kubeconfig(cluster, namespace)
            .map_err(|e| e.to_string())
    }
}

impl<H: KubeconfigSource + ?Sized> KubeconfigSource for Arc<H> {
    fn minified_kubeconfig(
        &self,
        cluster: &str,
        namespace: Option<&str>,
    ) -> Result<String, String> {
        (**self).minified_kubeconfig(cluster, namespace)
    }
}
