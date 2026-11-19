//! What the `*_ops` modules need of the app, named one piece at a time.
//!
//! [`AppCore`] holds everything, and a function that takes it says nothing
//! about which part it touches. Each trait here is one part. `AppCore` and a
//! shared `AppCore` implement them, so no caller changes, and a test can stand
//! in for the app with only the part under test.

use std::sync::Arc;

use k8s_core::cluster::ClusterHandle;

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
