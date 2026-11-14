//! Application state shared by every Kubernaut front end.
//!
//! Nothing here knows about a window system or an IPC layer, so the same
//! `AppCore` backs whichever UI is in front of it.

pub mod alert_ops;
pub mod audit;
mod boot;
pub mod clusters;
pub mod forward_ops;
pub mod helm_ops;
pub mod logging;
pub mod object_ops;
pub mod preferences;
pub mod present;
pub mod scan_cache;
pub mod security_ops;
mod state;

pub use boot::{Booted, boot};
pub use k8s_security as security;
pub use state::AppCore;
