//! The command palette: type a few letters, jump anywhere.
//!
//! It is the fast path to everything the sidebar offers, so a person who knows
//! what they want never has to scroll a list of sixty resource types.

use std::ops::Range;

use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Icon, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

const ROW_HEIGHT: Pixels = px(36.);
/// Rows visible at once; more scroll.
const VISIBLE_ROWS: usize = 8;
/// Matches kept, so a one-letter query over a large cluster stays cheap.
const MAX_RESULTS: usize = 100;

/// What choosing an entry does. Resolved by the app, which owns the state it
/// acts on.
#[derive(Clone)]
pub enum Command {
    Overview,
    Helm,
    Cluster(usize),
    Resource(SharedString),
    /// Add a namespace to the selection, or take it out.
    ToggleNamespace(SharedString),
    /// Back to every namespace.
    ClearNamespaces,
}

#[derive(Clone)]
pub struct Item {
    pub label: SharedString,
    /// Where it lives, shown on the right: a section or "Cluster".
    pub hint: SharedString,
    pub icon: IconName,
    pub command: Command,
}

pub enum PaletteEvent {
    Choose(Command),
    Close,
}

/// How well `query` matches `label`: lower is better, `None` is no match.
///
/// A prefix beats a word start, which beats a substring, which beats letters
/// found in order (`dpl` finds `Deployment`). Matching ignores case.
fn rank(query: &str, label: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let (query, label) = (query.to_lowercase(), label.to_lowercase());
    if label.starts_with(&query) {
        return Some(0);
    }
    let word_start = label
        .match_indices(&query)
        .any(|(at, _)| label[..at].ends_with([' ', '-', '/', '.']));
    if word_start {
        return Some(1);
    }
    if label.contains(&query) {
        return Some(2);
    }
    let mut letters = label.chars();
    query
        .chars()
        .all(|wanted| letters.any(|c| c == wanted))
        .then_some(3)
}

/// Indices of the items a query keeps, best first. Equal ranks keep the order
/// the items were given in, which puts pages and clusters ahead of resources.
fn matching(items: &[Item], query: &str) -> Vec<usize> {
    let query = query.trim();
    let mut hits: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| rank(query, &item.label).map(|rank| (rank, index)))
        .collect();
    hits.sort();
    hits.truncate(MAX_RESULTS);
    hits.into_iter().map(|(_, index)| index).collect()
}

pub struct PaletteView {
    footer: &'static str,
    input: Entity<InputState>,
    items: Vec<Item>,
    shown: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _input: Subscription,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    pub fn new(
        items: Vec<Item>,
        placeholder: &'static str,
        footer: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.refilter(cx),
                    InputEvent::PressEnter { .. } => this.choose(cx),
                    _ => {}
                },
            );
        input.update(cx, |input, cx| input.focus(window, cx));

        let shown = matching(&items, "");
        Self {
            footer,
            input,
            items,
            shown,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _input: subscription,
        }
    }

    /// Replace the entries, keeping the query and, as far as it still exists,
    /// the highlighted row. For a picker that stays open while choices change.
    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value();
        self.items = items;
        self.shown = matching(&self.items, &query);
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
        cx.notify();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value();
        self.shown = matching(&self.items, &query);
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.shown.len();
        if count == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn choose(&mut self, cx: &mut Context<Self>) {
        let item = self
            .shown
            .get(self.selected)
            .and_then(|&index| self.items.get(index));
        if let Some(item) = item {
            cx.emit(PaletteEvent::Choose(item.command.clone()));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => self.step(true, cx),
            "up" => self.step(false, cx),
            "escape" => cx.emit(PaletteEvent::Close),
            _ => {}
        }
    }
}

impl Render for PaletteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (popover, border, muted, selected_fill, on_selected, overlay) = (
            theme.popover,
            theme.border,
            theme.muted_foreground,
            theme.primary,
            theme.primary_foreground,
            theme.overlay,
        );

        let footer = self.footer;
        let rows = self.shown.len().clamp(1, VISIBLE_ROWS);
        let list = if self.shown.is_empty() {
            div()
                .px_4()
                .py_6()
                .text_sm()
                .text_color(muted)
                .child("Nothing matches.")
                .into_any_element()
        } else {
            uniform_list(
                "palette-items",
                self.shown.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    range
                        .filter_map(|position| {
                            let item = this.items.get(*this.shown.get(position)?)?;
                            let active = position == this.selected;
                            Some(
                                h_flex()
                                    .id(("palette-item", position))
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .px_3()
                                    .gap_3()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_sm()
                                    .when(active, |row| {
                                        row.bg(selected_fill).text_color(on_selected)
                                    })
                                    .child(Icon::new(item.icon).size_4())
                                    .child(div().flex_1().truncate().child(item.label.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .when(!active, |hint| hint.text_color(muted))
                                            .when(active, |hint| hint.opacity(0.85))
                                            .child(item.hint.clone()),
                                    )
                                    // Hovering moves the highlight, so the mouse and the
                                    // arrow keys agree on what Enter will open.
                                    .on_mouse_move(cx.listener(
                                        move |this, _: &MouseMoveEvent, _, cx| {
                                            if this.selected != position {
                                                this.selected = position;
                                                cx.notify();
                                            }
                                        },
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected = position;
                                        this.choose(cx);
                                    })),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .h(ROW_HEIGHT * rows as f32)
            .into_any_element()
        };

        // The dimmed backdrop swallows clicks, and a click on it closes.
        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(110.))
            .bg(overlay)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Close)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .child(
                v_flex()
                    .id("palette")
                    .w(px(560.))
                    .rounded_xl()
                    .border_1()
                    .border_color(border)
                    .bg(popover)
                    .shadow_lg()
                    .overflow_hidden()
                    // A click inside must not reach the backdrop and close it.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .px_3()
                            .py_2()
                            .gap_2()
                            .items_center()
                            .border_b_1()
                            .border_color(border)
                            .child(Icon::new(IconName::Search).size_4().text_color(muted))
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.input).appearance(false)),
                            ),
                    )
                    .child(list)
                    .child(
                        h_flex()
                            .px_3()
                            .py_1p5()
                            .gap_3()
                            .border_t_1()
                            .border_color(border)
                            .text_xs()
                            .text_color(muted)
                            .child("↑↓ move")
                            .child(footer)
                            .child("esc close"),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Item, matching, rank};
    use gpui_kit::assets::IconName;

    fn item(label: &str) -> Item {
        Item {
            label: label.to_string().into(),
            hint: "".into(),
            icon: IconName::Box,
            command: Command::Overview,
        }
    }

    #[test]
    fn closer_matches_rank_better() {
        let r = |label| rank("dep", label).unwrap();
        assert!(
            r("Deployment") < r("Helm deploy"),
            "prefix beats word start"
        );
        assert!(
            r("Helm deploy") < r("Undeployed"),
            "word start beats substring"
        );
        assert_eq!(rank("dpl", "Deployment"), Some(3), "letters in order");
        assert_eq!(rank("xyz", "Deployment"), None);
        assert_eq!(rank("POD", "pod"), Some(0), "case is ignored");
    }

    #[test]
    fn an_empty_query_keeps_the_given_order_and_nothing_is_dropped() {
        let items = [item("Overview"), item("Pod"), item("Service")];
        assert_eq!(matching(&items, ""), [0, 1, 2]);
        assert_eq!(matching(&items, "  "), [0, 1, 2], "blank is empty");
    }

    #[test]
    fn results_are_ordered_by_rank_then_by_position() {
        let items = [
            item("ReplicaSet"),
            item("Pod"),
            item("PodTemplate"),
            item("Job"),
        ];
        // "pod": two prefixes in given order; ReplicaSet and Job do not match.
        assert_eq!(matching(&items, "pod"), [1, 2]);
        // "e" is a prefix of nothing but a substring of three.
        let hits = matching(&items, "e");
        assert_eq!(hits.len(), 2);
        assert!(!hits.contains(&3));
    }
}
