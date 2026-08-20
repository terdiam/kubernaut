//! Helm release management.
//!
//! Reads come from the cluster's own release Secrets, so listing and inspecting
//! releases works without the helm binary. Writes (install, upgrade, rollback,
//! uninstall) go through helm itself — reimplementing chart rendering and
//! hooks would be a different product.

pub mod cli;
pub mod model;
pub mod store;

pub use cli::{Helm, UpgradeOptions, diff_manifests};
pub use model::{
    ChartResult, DocumentChange, HelmError, HelmInfo, Release, ReleaseDetail, ReleaseRevision,
    Repository, Result, UpgradeDiff,
};
