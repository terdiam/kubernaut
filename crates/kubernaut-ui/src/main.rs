mod app;
mod drawer;
mod emulator;
mod keys;
mod log_buffer;
mod logs;
mod rows;
mod table;
mod terminal;
mod yaml;

use std::{cell::Cell, process::ExitCode, rc::Rc, sync::Arc};

use gpui_kit::{
    component::{Theme, ThemeMode},
    *,
};

fn main() -> ExitCode {
    // Must run before any thread exists: it repairs PATH for exec credential
    // plugins, and mutating the environment is only sound single-threaded.
    let booted = k8s_app::boot();
    let core = Arc::new(booted.core);

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

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1440.), px(900.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Kubernaut".into()),
                ..Default::default()
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
