//! Application state shared by every Kubernaut front end.
//!
//! Nothing here knows about a window system or an IPC layer, so the same
//! `AppCore` backs whichever UI is in front of it.

pub mod alert_ops;
pub mod audit;
mod boot;
pub mod cluster_ops;
pub mod clusters;
pub mod forms;
pub mod forward_ops;
pub mod helm_ops;
pub mod logging;
pub mod manifest_ops;
pub mod metrics_ops;
pub mod object_ops;
pub mod preferences;
pub mod present;
pub mod scan_cache;
pub mod security_ops;
mod state;
pub mod templates;

pub use boot::{Booted, boot};
pub use k8s_security as security;
pub use state::AppCore;
