//! Cluster access layer for Kubernaut.
//!
//! Deliberately free of any UI/Tauri dependency so it can be unit-tested
//! headlessly and reused by a CLI later.

pub mod cluster;
pub mod discovery;
pub mod error;
pub mod jsonpath;
pub mod kubeconfig;
pub mod objects;
pub mod paths;
pub mod row;
pub mod schema;
pub mod watch;

pub use cluster::{ClusterHandle, ClusterId, ClusterManager, ClusterStatus, ConnectOptions};
pub use discovery::{ColumnDef, DiscoveryCache, ResourceDescriptor, ResourceGroup, resource_key};
pub use error::{CoreError, Result};
pub use kubeconfig::{ContextEntry, LoadedKubeconfig};
pub use row::{ColumnSpec, Row, RowHealth, RowProjector, TableSpec};
pub use schema::{ResourceSchema, SchemaCache};
pub use watch::{SubscriptionId, WatchBatch, WatchManager, WatchRequest, WatchState};
