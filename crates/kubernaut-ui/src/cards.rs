//! The card layout the detail screens share: a status card, bordered cards
//! whose header folds them, rows with thin lines between, and fields a click
//! copies. Plain functions over the theme, so any view can use them without
//! owning anything.
//!
//! Which cards are folded is one set for the whole app, kept in the
//! preferences, so folding Annotations once folds it everywhere.

use std::{collections::HashSet, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{Icon, Theme, h_flex, tooltip::Tooltip, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, details::pack, present::Tone};
use tokio::runtime::Handle;

use crate::ui::{toast_success, tone_color};

/// Longest value that still shares a line with another field.
const SHORT: usize = 22;

/// The cards the reader has folded, remembered across screens and sessions.
pub struct Folds {
    folded: HashSet<String>,
    core: Arc<AppCore>,
    runtime: Handle,
}

impl Folds {
    pub fn load(core: &Arc<AppCore>, runtime: &Handle) -> Self {
        Self {
            folded: core.folded_cards().into_iter().collect(),
            core: Arc::clone(core),
            runtime: runtime.clone(),
        }
    }

    pub fn is_folded(&self, id: &str) -> bool {
        self.folded.contains(id)
    }

    /// Fold a card, or open it if it is folded, and save the choice. Saving
    /// happens off the UI thread.
    pub fn toggle(&mut self, id: &'static str) {
        if !self.folded.remove(id) {
            self.folded.insert(id.to_string());
        }
        let mut folded: Vec<String> = self.folded.iter().cloned().collect();
        folded.sort();
        let core = Arc::clone(&self.core);
        self.runtime.spawn(async move {
            if let Err(reason) = core.update_preferences(|p| p.folded_cards = Some(folded)) {
                tracing::warn!(%reason, "could not save which cards are folded");
            }
        });
    }
}

/// A small count or status next to a card's title.
pub fn badge(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    div()
        .px_1p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_xs()
        .text_color(color)
        .child(text.into())
        .into_any_element()
}

/// How something is doing, as one card: a state, what it rests on, and a bar of
/// ready out of wanted when there is such a count.
pub fn status_card(
    tone: Tone,
    label: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    progress: Option<(i64, i64)>,
    theme: &Theme,
) -> Div {
    let colour = tone_color(tone, theme);
    v_flex()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(colour.opacity(0.4))
        .bg(colour.opacity(0.06))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().size(px(10.)).rounded_full().bg(colour))
                .child(
                    div()
                        .text_base()
                        .font_semibold()
                        .text_color(colour)
                        .child(label.into()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(detail.into()),
                ),
        )
        .when_some(
            progress.filter(|&(_, wanted)| wanted > 0),
            |card, (ready, wanted)| {
                let fraction = (ready as f32 / wanted as f32).clamp(0.0, 1.0);
                card.child(
                    div()
                        .h(px(6.))
                        .w_full()
                        .rounded_full()
                        .bg(colour.opacity(0.18))
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(colour)
                                .w(relative(fraction)),
                        ),
                )
            },
        )
}

/// A bordered card whose header folds it, with the rows under it separated by
/// thin lines. `on_toggle` is the header's click, which should call
/// [`Folds::toggle`] and redraw.
pub fn card(
    id: &'static str,
    title: impl Into<SharedString>,
    badges: Vec<AnyElement>,
    folded: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    rows: Vec<AnyElement>,
    theme: &Theme,
) -> Div {
    let (border, muted, hover) = (theme.border, theme.muted_foreground, theme.secondary);
    let title: SharedString = title.into();
    v_flex()
        .rounded_lg()
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .child(
            h_flex()
                .id(SharedString::from(format!("fold-{id}")))
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .cursor_pointer()
                .hover(|header| header.bg(hover))
                .tooltip(move |window, cx| {
                    Tooltip::new(if folded { "Show" } else { "Hide" }).build(window, cx)
                })
                .child(
                    Icon::new(if folded {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .size_3p5()
                    .text_color(muted),
                )
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(muted)
                        .child(title.to_uppercase()),
                )
                .children(badges)
                .on_click(on_toggle),
        )
        .when(!folded, |card| {
            card.children(rows.into_iter().map(|row| {
                div()
                    .border_t_1()
                    .border_color(border)
                    .px_3()
                    .py_1p5()
                    .child(row)
            }))
        })
}

/// `label` and `value`, the value wrapping rather than being cut. A click
/// copies it: an IP, a UID or an image is usually wanted elsewhere. `width` is
/// the label column; it is narrower when two fields share a line.
pub fn field(id: &str, label: &str, value: &str, width: f32, theme: &Theme) -> AnyElement {
    let (muted, hover) = (theme.muted_foreground, theme.secondary);
    let (copy, name) = (value.to_string(), label.to_string());
    h_flex()
        .id(SharedString::from(format!("{id}-{label}")))
        .gap_3()
        .items_start()
        .text_sm()
        .cursor_pointer()
        .rounded_sm()
        .hover(|row| row.bg(hover))
        .tooltip(|window, cx| Tooltip::new("Click to copy").build(window, cx))
        .child(
            div()
                .w(px(width))
                .flex_none()
                .text_color(muted)
                .child(label.to_string()),
        )
        .child(div().flex_1().min_w_0().child(value.to_string()))
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
            toast_success(window, cx, format!("Copied {name}"));
        })
        .into_any_element()
}

/// Fields as lines: two to a line when both values are short, so a card of
/// small facts does not run to a screen.
pub fn paired(id: &str, rows: &[&(String, String)], theme: &Theme) -> Vec<AnyElement> {
    pack(rows, |(_, value)| value.chars().count() <= SHORT)
        .into_iter()
        .map(|line| match line {
            [(a_label, a_value), (b_label, b_value)] => h_flex()
                .gap_4()
                .items_start()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(field(id, a_label, a_value, 130., theme)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(field(id, b_label, b_value, 104., theme)),
                )
                .into_any_element(),
            [(label, value)] => field(id, label, value, 130., theme),
            _ => div().into_any_element(),
        })
        .collect()
}
