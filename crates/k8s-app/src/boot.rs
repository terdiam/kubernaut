use k8s_core::{ClusterManager, LoadedKubeconfig};

use crate::{AppCore, clusters, logging, preferences::Preferences};

/// What start-up leaves behind. Keep `_log_guard` alive for the life of the
/// process: dropping it stops the log file being flushed, which loses exactly
/// the lines a crash report needs.
pub struct Booted {
    pub core: AppCore,
    pub _log_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Start-up shared by every front end. The order matters.
///
/// Call this first thing in `main`, before any thread is spawned: repairing
/// `PATH` mutates the process environment, which is only sound while the
/// process is still single-threaded.
pub fn boot() -> Booted {
    let log_guard = logging::init();

    // Contexts that authenticate through `aws`/`gcloud`/`az` shell out, and a
    // GUI launch does not inherit the login shell's PATH.
    let preferences = Preferences::load();
    let path_entries = hydrate_path(&preferences.extra_path_entries);
    tracing::info!(entries = path_entries.len(), "PATH resolved");

    // Only clusters the user added. Nothing is reachable because a file
    // happened to be in `~/.kube/config`.
    let clusters = match ClusterManager::from_managed(clusters::managed_files()) {
        Ok(manager) => manager,
        Err(err) => {
            tracing::warn!(%err, "starting with no clusters");
            ClusterManager::new(LoadedKubeconfig {
                config: Default::default(),
                sources: Vec::new(),
            })
        }
    };

    Booted {
        core: AppCore::new(clusters),
        _log_guard: log_guard,
    }
}

fn hydrate_path(extra: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            tracing::warn!(%err, "could not build startup runtime; keeping inherited PATH");
            return Vec::new();
        }
    };
    runtime.block_on(k8s_core::paths::hydrate_process_path(extra))
}
