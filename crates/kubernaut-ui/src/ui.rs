//! Small widgets several views share.

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, Theme},
    prelude::FluentBuilder,
    *,
};
use k8s_app::present::Tone;

/// A bordered button label. Whether it does anything is the caller's to wire:
/// attach `on_click` only when `enabled`, so a dimmed chip is also inert.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .border_1()
        .border_color(theme.border)
        .when(active, |chip| chip.bg(theme.secondary).font_semibold())
        .when(enabled, |chip| chip.cursor_pointer())
        .when(!enabled, |chip| {
            chip.opacity(0.4).text_color(theme.muted_foreground)
        })
        .child(label.into())
}

pub fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Ok => theme.success,
        Tone::Pending => theme.info,
        Tone::Warn => theme.warning,
        Tone::Error => theme.danger,
    }
}

/// A line of muted or error text for a loading or failed state.
pub fn message(text: impl Into<SharedString>, color: Hsla) -> Div {
    div().p_4().text_sm().text_color(color).child(text.into())
}

/// Colours a view reads from the theme, copied so the theme borrow ends before
/// the view starts using `cx` again.
#[derive(Clone, Copy)]
pub struct Palette {
    pub muted: Hsla,
    pub danger: Hsla,
    pub border: Hsla,
}

impl Palette {
    pub fn of(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            muted: theme.muted_foreground,
            danger: theme.danger,
            border: theme.border,
        }
    }
}
