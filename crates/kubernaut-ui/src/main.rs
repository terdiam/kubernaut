mod activity;
mod add_cluster;
mod alerts;
mod app;
mod categories;
mod charts;
mod crashes;
mod create;
mod diagnose;
mod diff;
mod drawer;
mod emulator;
mod form;
mod gitops;
mod helm;
mod icons;
mod keys;
mod log_buffer;
mod logs;
mod metrics;
mod namespaces;
mod node_edit;
mod overview;
mod palette;
mod ports;
mod quota;
mod release;
mod row_actions;
mod rows;
mod security;
mod settings;
mod table;
mod terminal;
mod theme;
mod topology;
mod ui;
mod upgrade;
mod usage;
mod validation;
mod yaml;

use std::{cell::Cell, process::ExitCode, rc::Rc, sync::Arc};

use gpui_kit::*;

fn main() -> ExitCode {
    // Must run before any thread exists: it repairs PATH for exec credential
    // plugins, and mutating the environment is only sound single-threaded.
    let booted = k8s_app::boot();
    let core = Arc::new(booted.core);
    // A packaged build carries helm, trivy and kustomize beside the executable.
    // A development build has none there and falls back to the ones on PATH.
    core.set_helm_sidecar_dir(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf)),
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("kubernaut-tokio")
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(%err, "could not start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    let handle = runtime.handle().clone();

    let opened = Rc::new(Cell::new(false));
    let opened_in_app = Rc::clone(&opened);

    gpui_kit::application()
        .with_assets(icons::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::apply(None, cx);
            cx.bind_keys([KeyBinding::new(
                "cmd-k",
                app::TogglePalette,
                Some(app::KEY_CONTEXT),
            )]);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1440.), px(900.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kubernaut".into()),
                    // The toolbar is drawn by the app, so the window's own title
                    // bar steps aside and leaves only the traffic lights.
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(16.))),
                }),
                ..Default::default()
            };

            match gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| app::AppView::new(core, handle, window, cx))
            }) {
                Ok(_) => {
                    opened_in_app.set(true);
                    cx.activate(true);
                }
                Err(err) => {
                    tracing::error!(%err, "could not open the window");
                    cx.quit();
                }
            }
        });

    // `booted` holds the log writer; it must outlive the app so the last lines
    // are flushed.
    drop(booted._log_guard);
    drop(runtime);

    if opened.get() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
