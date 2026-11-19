//! Small widgets several views share.

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, Theme, WindowExt, h_flex, notification::Notification, tooltip::Tooltip,
    },
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
    chip_base(id, enabled, active, theme).child(label.into())
}

/// A chip with an icon before its label.
pub fn chip_icon(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    enabled: bool,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    chip_base(id, enabled, active, theme)
        .flex()
        .items_center()
        .gap_1()
        .child(Icon::new(icon).size_3())
        .child(label.into())
}

fn chip_base(
    id: impl Into<ElementId>,
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

/// A small rounded status badge: a dot and a label in one colour on a faint
/// tint of it.
pub fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_xs()
        .text_color(color)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(text.into())
}

/// The container for a segmented control: a rounded track holding a row of
/// [`segment`]s, like the one in Finder's view switcher.
pub fn segmented_bar(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .bg(theme.secondary)
}

fn segment_base(id: impl Into<ElementId>, active: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .cursor_pointer()
        .when(active, |segment| {
            segment
                .bg(theme.background)
                .font_semibold()
                .shadow_sm()
                .text_color(theme.foreground)
        })
        .when(!active, |segment| {
            segment
                .text_color(theme.muted_foreground)
                .hover(|segment| segment.text_color(theme.foreground))
        })
}

/// One option of a segmented control. The caller attaches `on_click`.
pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    segment_base(id, active, theme).child(label.into())
}

/// A segment with an icon before its label.
pub fn segment_icon(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    segment_base(id, active, theme)
        .flex()
        .items_center()
        .gap_1p5()
        .child(Icon::new(icon).size_3())
        .child(label.into())
}

/// A toolbar button drawn as an icon alone, with its action in a tooltip. A
/// toggle that is on takes the accent colour and a tinted background. The
/// caller attaches `on_click`.
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tip: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let tip: SharedString = tip.into();
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .when(active, |button| {
            button
                .bg(theme.secondary)
                .border_color(theme.primary.opacity(0.6))
                .text_color(theme.primary)
        })
        .when(!active, |button| {
            button
                .text_color(theme.muted_foreground)
                .hover(|button| button.bg(theme.secondary).text_color(theme.foreground))
        })
        .child(Icon::new(icon).size(px(16.)))
}

/// A tab drawn as an icon alone, with its name in a tooltip. Larger than the
/// icon of a labelled segment, because the shape is all there is to go by; the
/// open one is lifted and takes the foreground colour.
pub fn tab_icon(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    div()
        .id(id)
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when(active, |tab| {
            tab.bg(theme.background)
                .shadow_sm()
                .text_color(theme.primary)
        })
        .when(!active, |tab| {
            tab.text_color(theme.muted_foreground).hover(|tab| {
                tab.bg(theme.background.opacity(0.6))
                    .text_color(theme.foreground)
            })
        })
        .child(Icon::new(icon).size(px(18.)))
}

/// How loudly a button asks to be pressed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Plain,
    /// The main action of a group, filled with the accent colour.
    Primary,
    /// An action that deletes or replaces something.
    Danger,
}

fn button_base(
    id: impl Into<ElementId>,
    enabled: bool,
    emphasis: Emphasis,
    theme: &Theme,
) -> Stateful<Div> {
    let (fill, fill_hover) = match emphasis {
        Emphasis::Plain => (theme.secondary, theme.secondary_hover),
        Emphasis::Primary => (theme.primary, theme.primary_hover),
        Emphasis::Danger => (theme.danger, theme.danger.opacity(0.85)),
    };
    let text = if emphasis == Emphasis::Plain {
        theme.foreground
    } else {
        theme.primary_foreground
    };
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .text_xs()
        .font_semibold()
        .bg(fill)
        .text_color(text)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|button| button.bg(fill_hover))
        })
        .when(!enabled, |button| button.opacity(0.4))
}

/// A button. Like [`chip`], the caller attaches `on_click` when it is enabled.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    emphasis: Emphasis,
    theme: &Theme,
) -> Stateful<Div> {
    button_base(id, enabled, emphasis, theme).child(label.into())
}

/// A button with an icon before its label, for the actions people look for by
/// shape: refresh, preview, apply, open.
pub fn button_icon(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    enabled: bool,
    emphasis: Emphasis,
    theme: &Theme,
) -> Stateful<Div> {
    button_base(id, enabled, emphasis, theme)
        .flex()
        .items_center()
        .gap_1p5()
        .child(Icon::new(icon).size_3p5())
        .child(label.into())
}

/// The title of a page: its icon, then its name.
pub fn page_title(icon: IconName, text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .font_semibold()
        .child(Icon::new(icon).size_4().text_color(theme.muted_foreground))
        .child(text.into())
}

/// A brief confirmation that something worked. It fades on its own.
pub fn toast_success(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::success(message), cx);
}

/// A failure. It stays until dismissed, because the reason is worth reading.
pub fn toast_error(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    reason: impl Into<SharedString>,
) {
    window.push_notification(Notification::error(reason).title(title).autohide(false), cx);
}

/// One cell of a list drawn as a grid: padded, clipped, and with a thin line on
/// its right edge unless it is the last. `width` is fixed in pixels; `None`
/// takes what the others leave. Header and rows build their cells from this, so
/// the columns line up.
pub fn grid_cell(theme: &Theme, width: Option<f32>, last: bool) -> Div {
    let cell = div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .overflow_hidden()
        .min_w_0();
    let cell = match width {
        Some(width) => cell.w(px(width)).flex_none(),
        None => cell.flex_1(),
    };
    if last {
        cell
    } else {
        cell.border_r_1().border_color(theme.border)
    }
}

/// The column names above a grid list, in the same cells as its rows.
pub fn grid_header(theme: &Theme, columns: &[(&'static str, Option<f32>)]) -> Div {
    h_flex()
        .w_full()
        .h(px(28.))
        .flex_none()
        .border_y_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.5))
        .text_xs()
        .font_semibold()
        .text_color(theme.muted_foreground)
        .children(columns.iter().enumerate().map(|(index, (label, width))| {
            grid_cell(theme, *width, index + 1 == columns.len()).child(*label)
        }))
}

/// A thin bar showing how far something has got. With a fraction it fills from
/// the left; without one (a step of unknown length) a short stripe sweeps
/// across, driven by the clock, so the caller only has to repaint now and then.
pub fn progress_bar(fraction: Option<f32>, width: DefiniteLength, theme: &Theme) -> Div {
    let track = div()
        .h(px(4.))
        .w(width)
        .flex_none()
        .rounded_full()
        .overflow_hidden()
        .bg(theme.border);
    match fraction {
        Some(fraction) => track.child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0., 1.)))
                .rounded_full()
                .bg(theme.primary),
        ),
        None => {
            const STRIPE: f32 = 0.3;
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() % 1400);
            let at = millis as f32 / 1400. * (1. + STRIPE) - STRIPE;
            track.child(
                div()
                    .h_full()
                    .w(relative(STRIPE))
                    .ml(relative(at.max(0.)))
                    .rounded_full()
                    .bg(theme.primary),
            )
        }
    }
}
