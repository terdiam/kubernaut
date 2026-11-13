//! Application state shared by every Kubernaut front end.
//!
//! Nothing here knows about a window system or an IPC layer, so the same
//! `AppCore` backs whichever UI is in front of it.

pub mod audit;
mod boot;
pub mod clusters;
pub mod logging;
pub mod preferences;
pub mod present;
pub mod scan_cache;
mod state;

pub use boot::{Booted, boot};
pub use state::AppCore;
