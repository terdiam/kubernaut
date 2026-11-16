//! Minimise, maximise and close for a window whose title bar the app draws.
//!
//! macOS draws its own traffic lights. On Windows and Linux the system caption
//! is gone (the toolbar is the title bar), so without these there is no way to
//! minimise or close except the keyboard. Windows decides what a click on the
//! three areas means; Linux needs the click handled here.

use gpui_kit::{
    assets::IconName,
    component::{ActiveTheme, Icon, h_flex},
    prelude::FluentBuilder,
    *,
};

/// Width of one button, as on a native Windows caption.
const BUTTON_WIDTH: Pixels = px(46.);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Control {
    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "window-minimize",
            Self::Maximize => "window-maximize",
            Self::Restore => "window-restore",
            Self::Close => "window-close",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Maximize => IconName::WindowMaximize,
            Self::Restore => IconName::WindowRestore,
            Self::Close => IconName::WindowClose,
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }
}

fn button(control: Control, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, hover_text) = if control == Control::Close {
        (theme.danger, theme.danger_foreground)
    } else {
        (theme.secondary_hover, theme.foreground)
    };
    div()
        .id(control.id())
        .w(BUTTON_WIDTH)
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.foreground)
        .hover(|style| style.bg(hover).text_color(hover_text))
        // On Windows the system hit-tests these areas, so a click does what the
        // caption button does, including the snap-layout flyout on Maximize.
        .when(cfg!(windows), |button| {
            button.window_control_area(control.area())
        })
        .when(cfg!(target_os = "linux"), |button| {
            button
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match control {
                        Control::Minimize => window.minimize_window(),
                        Control::Maximize | Control::Restore => window.zoom_window(),
                        Control::Close => window.remove_window(),
                    }
                })
        })
        .child(Icon::new(control.icon()).size_3p5())
}

/// The three buttons, or `None` where the system draws them (macOS, and Linux
/// when the window manager supplies its own title bar).
pub fn window_controls(window: &Window, cx: &App) -> Option<AnyElement> {
    if cfg!(target_os = "macos") {
        return None;
    }
    #[cfg(target_os = "linux")]
    if !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return None;
    }
    // A tiling window manager may support neither; close is always offered.
    let supported = window.window_controls();
    let maximised = window.is_maximized();
    Some(
        h_flex()
            .id("window-controls")
            .h_full()
            .flex_none()
            .items_center()
            .when(supported.minimize, |row| {
                row.child(button(Control::Minimize, cx))
            })
            .when(supported.maximize, |row| {
                row.child(button(
                    if maximised {
                        Control::Restore
                    } else {
                        Control::Maximize
                    },
                    cx,
                ))
            })
            .child(button(Control::Close, cx))
            .into_any_element(),
    )
}

/// Whether the toolbar has to leave room on its left for macOS's traffic lights.
pub const fn leaves_room_for_traffic_lights() -> bool {
    cfg!(target_os = "macos")
}
