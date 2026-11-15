// A release build on Windows is a window app, not a console one, so it does not
// open a terminal behind itself. Debug builds keep the console for their logs.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod activity;
mod add_cluster;
mod alerts;
mod app;
mod categories;
mod charts;
mod cluster_info;
mod crashes;
mod create;
mod details;
mod diagnose;
mod diff;
mod drain;
mod drawer;
mod emulator;
mod form;
mod gitops;
mod helm;
mod icons;
mod keys;
mod log_buffer;
mod logs;
mod menu;
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

/// Run `act` on the app view in its window.
fn with_app(
    handle: AnyWindowHandle,
    view: &Entity<app::AppView>,
    cx: &mut App,
    act: impl FnOnce(&mut app::AppView, &mut Window, &mut Context<app::AppView>),
) {
    let view = view.clone();
    handle
        .update(cx, |_, window, cx| {
            view.update(cx, |view, cx| act(view, window, cx));
        })
        .ok();
}

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
            cx.bind_keys([
                KeyBinding::new("cmd-k", app::TogglePalette, Some(app::KEY_CONTEXT)),
                KeyBinding::new("cmd-,", app::ShowSettings, None),
                // The system-wide ones work whatever has focus.
                KeyBinding::new("cmd-q", app::QuitApp, None),
                KeyBinding::new("cmd-h", app::HideApp, None),
                KeyBinding::new("cmd-alt-h", app::HideOtherApps, None),
            ]);
            cx.on_action(|_: &app::QuitApp, cx| cx.quit());
            cx.on_action(|_: &app::HideApp, cx| cx.hide());
            cx.on_action(|_: &app::HideOtherApps, cx| cx.hide_other_apps());
            cx.on_action(|_: &app::ShowAllApps, cx| cx.unhide_other_apps());
            cx.set_menus(menu::menus());

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
                Ok((handle, view)) => {
                    // Handled here rather than by the focused view: a menu item is
                    // greyed out when nothing in the focus path takes its action,
                    // which is the case after a dialog closes. The window is busy
                    // dispatching the action, so the work waits for it to finish.
                    let (about, updates, settings) = (view.clone(), view.clone(), view);
                    cx.on_action(move |_: &app::About, cx| {
                        let view = about.clone();
                        cx.defer(move |cx| {
                            with_app(handle, &view, cx, |view, window, cx| {
                                view.show_about(window, cx);
                            });
                        });
                    });
                    cx.on_action(move |_: &app::CheckForUpdates, cx| {
                        let view = updates.clone();
                        cx.defer(move |cx| {
                            with_app(handle, &view, cx, |view, window, cx| {
                                view.check_for_updates(window, cx);
                            });
                        });
                    });
                    cx.on_action(move |_: &app::ShowSettings, cx| {
                        let view = settings.clone();
                        cx.defer(move |cx| {
                            with_app(handle, &view, cx, |view, window, cx| {
                                view.open_settings(window, cx);
                            });
                        });
                    });
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
