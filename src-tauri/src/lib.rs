mod audit;
mod clusters;
mod commands;
mod error;
mod helm_commands;
mod logging;
mod metrics_commands;
mod ops_commands;
mod preferences;
mod security_commands;
mod state;

use k8s_core::ClusterManager;
use state::AppState;

/// Entry point shared by the desktop binary.
pub fn run() {
    // Must happen before WebKitGTK initializes, which starts somewhere inside
    // `tauri::Builder::run()` below — see the function for why.
    #[cfg(target_os = "linux")]
    apply_webkit_workarounds();

    // Held for the life of the process: dropping the guard stops the log being
    // flushed, which loses exactly the lines a crash report needs.
    let _log_guard = logging::init();

    // PATH must be repaired before anything touches a kubeconfig: contexts that
    // authenticate through `aws`/`gcloud`/`az` shell out, and a GUI launch does
    // not inherit the login shell's PATH. Done here, on the main thread and
    // before Tauri spawns anything, because mutating the process environment is
    // only sound while the process is still single-threaded.
    let preferences = preferences::Preferences::load();
    let path_entries = hydrate_path(&preferences.extra_path_entries);
    tracing::info!(entries = path_entries.len(), "PATH resolved");

    // Only clusters the user added. Nothing is reachable because a file
    // happened to be in `~/.kube/config`.
    let clusters = match ClusterManager::from_managed(clusters::managed_files()) {
        Ok(manager) => manager,
        Err(err) => {
            tracing::warn!(%err, "starting with no clusters");
            ClusterManager::new(k8s_core::LoadedKubeconfig {
                config: Default::default(),
                sources: Vec::new(),
            })
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::new(clusters))
        .setup(|app| {
            // Packaged builds ship helm next to the app resources; a dev build
            // has no sidecar and falls back to the user's own helm.
            use tauri::Manager;
            let sidecar = app
                .path()
                .resource_dir()
                .ok()
                .map(|dir| dir.join("binaries"));
            app.state::<AppState>().set_helm_sidecar_dir(sidecar);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_contexts,
            commands::reload_kubeconfig,
            commands::connect_cluster,
            commands::disconnect_cluster,
            commands::cluster_status,
            commands::discover,
            commands::list_namespaces,
            commands::resource_schema,
            commands::watch_resource,
            commands::stop_watch,
            commands::resync_watch,
            commands::watch_state,
            commands::get_object,
            commands::diagnostics,
            commands::get_preferences,
            commands::last_crash,
            commands::list_audit_entries,
            commands::managed_kubeconfigs,
            commands::system_kubeconfig_contexts,
            commands::import_system_contexts,
            commands::cluster_profile,
            commands::set_cluster_profile,
            commands::preview_kubeconfig,
            commands::read_kubeconfig_file,
            commands::import_kubeconfig,
            commands::remove_kubeconfig,
            commands::set_preferences,
            ops_commands::start_logs,
            ops_commands::stop_logs,
            ops_commands::pod_containers,
            ops_commands::workload_pods,
            ops_commands::log_snapshot,
            ops_commands::open_terminal,
            ops_commands::open_ephemeral_terminal,
            ops_commands::open_node_shell,
            ops_commands::open_local_shell,
            ops_commands::terminal_write,
            ops_commands::terminal_resize,
            ops_commands::close_terminal,
            ops_commands::start_forward,
            ops_commands::stop_forward,
            ops_commands::list_forwards,
            ops_commands::target_ports,
            ops_commands::preview_edit,
            ops_commands::apply_edit,
            ops_commands::scale_workload,
            ops_commands::current_scale,
            ops_commands::restart_workload,
            ops_commands::set_node_cordoned,
            ops_commands::drain_node,
            ops_commands::delete_object,
            ops_commands::evict_pod,
            ops_commands::object_events,
            ops_commands::pod_events,
            ops_commands::related_resources,
            ops_commands::diagnose_object,
            ops_commands::delete_objects,
            ops_commands::restart_workloads,
            ops_commands::export_objects,
            ops_commands::export_objects_to_file,
            ops_commands::lookup_options,
            ops_commands::plan_manifest,
            ops_commands::apply_manifest,
            ops_commands::gitops_survey,
            ops_commands::gitops_reconcile,
            ops_commands::gitops_set_suspended,
            metrics_commands::cluster_overview,
            metrics_commands::overview_history,
            metrics_commands::namespace_usage,
            metrics_commands::namespace_quota,
            metrics_commands::object_metrics,
            metrics_commands::metrics_sources,
            metrics_commands::topology,
            metrics_commands::node_summaries,
            metrics_commands::pod_usages,
            metrics_commands::workload_sizing,
            helm_commands::helm_info,
            helm_commands::helm_releases,
            helm_commands::helm_history,
            helm_commands::helm_release_detail,
            helm_commands::helm_repos,
            helm_commands::helm_repo_add,
            helm_commands::helm_repo_remove,
            helm_commands::helm_repo_update,
            helm_commands::helm_search,
            helm_commands::helm_chart_values,
            helm_commands::helm_preview_upgrade,
            helm_commands::helm_upgrade,
            helm_commands::helm_rollback,
            helm_commands::helm_uninstall,
            security_commands::security_scan,
            security_commands::posture_scan,
            security_commands::rbac_scan,
            security_commands::cluster_images,
            security_commands::vulnerability_scanner,
            security_commands::vulnerability_scan,
            security_commands::download_vulnerability_database,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Kubernaut");
}

/// Work around a WebKitGTK bug where the webview paints nothing at all: an
/// empty window, correct size and title, no error anywhere — reported on
/// Arch and other rolling distros, on GPU/driver combinations where
/// WebKitGTK's hardware-accelerated compositor (its DMA-BUF renderer, in
/// recent versions) never produces a frame. Forcing the software compositor
/// trades a little GPU offload for a window that actually shows content,
/// which is the standard fix for this class of bug.
///
/// Only sets a variable the user has not already set themselves, so someone
/// who has their own workaround in place — or wants to try without this one —
/// is not overridden.
///
/// Must run before WebKitGTK reads these on startup, and while the process is
/// still single-threaded — `run()` calls this before anything else.
#[cfg(target_os = "linux")]
fn apply_webkit_workarounds() {
    for (key, value) in [
        ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
    ] {
        if std::env::var_os(key).is_none() {
            // SAFETY: the first thing `run()` does, before any thread exists
            // besides this one.
            unsafe { std::env::set_var(key, value) };
        }
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
