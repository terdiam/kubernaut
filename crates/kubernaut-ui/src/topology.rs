//! Topology: how traffic and ownership connect in one namespace.
//!
//! Left to right: ingresses route to services, services select pods, workloads
//! own them, and pods run on nodes. A service that selects nothing, or a
//! workload with no ready pod, is the kind of thing that is invisible in lists
//! and obvious in a picture.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, Theme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, metrics_ops, present::Tone};
use k8s_metrics::{Topology, TopologyNode};
use tokio::runtime::Handle;

use crate::{
    cards,
    drawer::Target,
    ui::{Emphasis, Palette, button, message, page_title, pill},
};

const CARD_W: f32 = 200.;
const CARD_H: f32 = 54.;
const COLUMN_GAP: f32 = 90.;
const ROW_GAP: f32 = 10.;
const PAD: f32 = 20.;
/// Room above the cards for each column's heading.
const HEADING: f32 = 22.;

/// Left to right, the way traffic and ownership run.
const COLUMNS: [&str; 5] = ["Ingress", "Service", "Workload", "Pod", "Node"];

/// How far the pointer must move before a press on a card is a drag and not a
/// click, in pixels.
const DRAG_SLOP: f32 = 4.;
/// Space between the dots that run along a line, and how fast they run, in
/// pixels and pixels per second.
const DOT_SPACING: f32 = 48.;
const DOT_SPEED: f32 = 70.;
/// How often the dots are redrawn.
const FRAME: Duration = Duration::from_millis(33);

pub enum TopologyEvent {
    /// Show the object behind a card.
    Open(Target),
}

/// Where each node goes, and what is drawn between them.
#[derive(Debug, PartialEq)]
struct Layout {
    /// Top-left of each node's card, by the node's index; `None` for a kind
    /// with no column.
    positions: Vec<Option<(f32, f32)>>,
    /// Pairs of node indices to join.
    links: Vec<(usize, usize)>,
    /// A heading and its column's left edge.
    headings: Vec<(&'static str, f32)>,
    width: f32,
    height: f32,
}

fn column_of(kind: &str) -> Option<usize> {
    COLUMNS.iter().position(|column| *column == kind)
}

/// Lay the nodes out in columns, ordering each column by where its neighbours
/// to the left ended up, so lines run mostly straight rather than crossing.
fn layout(nodes: &[TopologyNode], edges: &[k8s_metrics::TopologyEdge]) -> Layout {
    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(at, node)| (node.id.as_str(), at))
        .collect();
    let links: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|edge| {
            Some((
                *index.get(edge.from.as_str())?,
                *index.get(edge.to.as_str())?,
            ))
        })
        .collect();

    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); COLUMNS.len()];
    for (at, node) in nodes.iter().enumerate() {
        if let Some(column) = column_of(&node.kind) {
            columns[column].push(at);
        }
    }

    // Row of each node once placed, to order the next column by.
    let mut row_of: Vec<Option<usize>> = vec![None; nodes.len()];
    for column in &mut columns {
        column.sort_by(|&a, &b| {
            let key = |at: usize| {
                // Average row of the neighbours already placed, either side.
                let rows: Vec<f32> = links
                    .iter()
                    .filter_map(|&(from, to)| {
                        let other = if from == at {
                            to
                        } else if to == at {
                            from
                        } else {
                            return None;
                        };
                        row_of[other].map(|row| row as f32)
                    })
                    .collect();
                (!rows.is_empty()).then(|| rows.iter().sum::<f32>() / rows.len() as f32)
            };
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| nodes[a].name.cmp(&nodes[b].name))
        });
        for (row, &at) in column.iter().enumerate() {
            row_of[at] = Some(row);
        }
    }

    let used: Vec<usize> = (0..COLUMNS.len())
        .filter(|&column| !columns[column].is_empty())
        .collect();
    let mut positions = vec![None; nodes.len()];
    let mut headings = Vec::with_capacity(used.len());
    let mut tallest = 0;
    for (slot, &column) in used.iter().enumerate() {
        let x = PAD + slot as f32 * (CARD_W + COLUMN_GAP);
        headings.push((COLUMNS[column], x));
        tallest = tallest.max(columns[column].len());
        for (row, &at) in columns[column].iter().enumerate() {
            positions[at] = Some((x, PAD + HEADING + row as f32 * (CARD_H + ROW_GAP)));
        }
    }
    let links = links
        .into_iter()
        .filter(|&(from, to)| positions[from].is_some() && positions[to].is_some())
        .collect();
    let columns_shown = used.len().max(1) as f32;
    Layout {
        positions,
        links,
        headings,
        width: PAD * 2. + columns_shown * CARD_W + (columns_shown - 1.) * COLUMN_GAP,
        height: PAD * 2. + HEADING + tallest as f32 * (CARD_H + ROW_GAP),
    }
}

/// Where along a line (0 at its start, 1 at its end) each travelling dot is
/// `seconds` into the animation. Longer lines carry more dots, so the spacing
/// and the speed look the same on every line.
fn flow(length: f32, seconds: f32) -> impl Iterator<Item = f32> {
    let count = ((length / DOT_SPACING).round() as usize).max(1);
    let shift = if length > 0. {
        seconds * DOT_SPEED / length
    } else {
        0.
    };
    (0..count).map(move |at| (shift + at as f32 / count as f32).fract())
}

/// A point on the cubic Bézier through `p` at `t`.
fn bezier(p: [(f32, f32); 4], t: f32) -> (f32, f32) {
    let u = 1. - t;
    let weight = [u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t];
    let at = |axis: fn((f32, f32)) -> f32| (0..4).map(|i| weight[i] * axis(p[i])).sum::<f32>();
    (at(|point| point.0), at(|point| point.1))
}

fn health_color(health: &str, theme: &Theme) -> Hsla {
    match health {
        "ok" => theme.success,
        "pending" => theme.info,
        "warning" => theme.warning,
        "error" => theme.danger,
        _ => theme.muted_foreground,
    }
}

/// One card, with what it needs to draw and to open the object.
struct Card {
    x: f32,
    y: f32,
    /// Where the layout put it, to go back to after it was dragged.
    home: (f32, f32),
    name: SharedString,
    detail: SharedString,
    health: SharedString,
    target: Option<Target>,
}

struct Drawn {
    cards: Vec<Card>,
    links: Vec<(usize, usize)>,
    headings: Vec<(&'static str, f32)>,
    width: f32,
    height: f32,
    truncated: bool,
}

/// How the drawn objects are doing, for the card above the graph: the worst
/// state first, and how many are healthy out of all of them.
fn graph_status(drawn: &Drawn) -> (Tone, String, String, (i64, i64)) {
    let count = |wanted: &str| {
        drawn
            .cards
            .iter()
            .filter(|card| &*card.health == wanted)
            .count()
    };
    let (ok, errors) = (count("ok"), count("error"));
    let total = drawn.cards.len();
    let waiting = total - ok - errors;
    let (tone, label) = if errors > 0 {
        (Tone::Error, format!("{errors} unhealthy"))
    } else if waiting > 0 {
        (Tone::Warn, format!("{waiting} not ready"))
    } else {
        (Tone::Ok, "All healthy".to_string())
    };
    let links = drawn.links.len();
    let detail = format!(
        "{ok} of {total} objects healthy · {links} link{}",
        if links == 1 { "" } else { "s" }
    );
    (tone, label, detail, (ok as i64, total as i64))
}

fn drawn(topology: &Topology, cluster: &SharedString) -> Drawn {
    let layout = layout(&topology.nodes, &topology.edges);
    let mut cards = Vec::new();
    // Card index by node index, so links keep pointing at the right card.
    let mut at_card = vec![None; topology.nodes.len()];
    for (node_at, node) in topology.nodes.iter().enumerate() {
        let Some((x, y)) = layout.positions[node_at] else {
            continue;
        };
        at_card[node_at] = Some(cards.len());
        cards.push(Card {
            x,
            y,
            home: (x, y),
            name: node.name.clone().into(),
            detail: node.detail.clone().unwrap_or_default().into(),
            health: node.health.clone().into(),
            target: node.resource.as_ref().map(|resource| Target {
                cluster: cluster.clone(),
                resource: resource.clone().into(),
                kind: node
                    .sub_kind
                    .clone()
                    .unwrap_or_else(|| node.kind.clone())
                    .into(),
                namespace: node.namespace.clone().map(Into::into),
                name: node.name.clone().into(),
            }),
        });
    }
    let links = layout
        .links
        .iter()
        .filter_map(|&(from, to)| Some((at_card[from]?, at_card[to]?)))
        .collect();
    Drawn {
        cards,
        links,
        headings: layout.headings,
        width: layout.width,
        height: layout.height,
        truncated: topology.truncated,
    }
}

enum Load {
    Loading,
    Ready(Drawn),
    Failed(SharedString),
}

/// What a held mouse button is doing on the page.
enum Drag {
    /// Moving a card: where the pointer and the card were when it was pressed.
    Card {
        index: usize,
        pointer: Point<Pixels>,
        origin: (f32, f32),
        moved: bool,
    },
    /// Panning the view: where the pointer and the scroll position were.
    Pan {
        pointer: Point<Pixels>,
        offset: Point<Pixels>,
    },
}

pub struct TopologyView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    /// Namespaces to pick from.
    known: Vec<SharedString>,
    namespace: Option<SharedString>,
    load: Load,
    /// The card whose lines are drawn strongly.
    focus: Option<usize>,
    scroll: ScrollHandle,
    drag: Option<Drag>,
    /// A card was just dragged, so the click that ends the drag opens nothing.
    just_dragged: bool,
    /// The animation's clock.
    started: Instant,
}

impl EventEmitter<TopologyEvent> for TopologyView {}

/// The first namespace of the selection that this cluster has.
fn followed(known: &[SharedString], scope: &[SharedString]) -> Option<SharedString> {
    scope.iter().find(|ns| known.contains(ns)).cloned()
}

impl TopologyView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        known: Vec<SharedString>,
        preferred: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        // A namespace the user is already looking at, else `default`, else the
        // first: some namespace has to be drawn, and the whole cluster is a mass.
        let namespace = preferred
            .filter(|ns| known.contains(ns))
            .or_else(|| known.iter().find(|ns| &***ns == "default").cloned())
            .or_else(|| known.first().cloned());
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            known,
            namespace,
            load: Load::Loading,
            focus: None,
            scroll: ScrollHandle::new(),
            drag: None,
            just_dragged: false,
            started: Instant::now(),
        };
        view.refresh(cx);
        // Redraw only while there are lines to animate, and for as long as the
        // page exists: the view going away ends the loop.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.animating() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(namespace) = self.namespace.clone() else {
            return;
        };
        self.load = Load::Loading;
        self.focus = None;
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let shown_cluster = self.cluster.clone();
        let task = self.runtime.spawn(async move {
            metrics_ops::topology(&core, &cluster, &[namespace.to_string()]).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.load = match result {
                    Ok(topology) => Load::Ready(drawn(&topology, &shown_cluster)),
                    Err(reason) => Load::Failed(reason.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn animating(&self) -> bool {
        matches!(&self.load, Load::Ready(drawn) if !drawn.links.is_empty())
    }

    fn press_card(&mut self, index: usize, event: &MouseDownEvent) {
        let Load::Ready(drawn) = &self.load else {
            return;
        };
        let Some(card) = drawn.cards.get(index) else {
            return;
        };
        self.just_dragged = false;
        self.drag = Some(Drag::Card {
            index,
            pointer: event.position,
            origin: (card.x, card.y),
            moved: false,
        });
    }

    fn press_background(&mut self, event: &MouseDownEvent) {
        self.drag = Some(Drag::Pan {
            pointer: event.position,
            offset: self.scroll.offset(),
        });
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            // The release happened where nothing was listening.
            self.release(cx);
            return;
        }
        match &mut self.drag {
            Some(Drag::Card {
                index,
                pointer,
                origin,
                moved,
            }) => {
                let delta = event.position - *pointer;
                let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                if !*moved && dx.abs() + dy.abs() < DRAG_SLOP {
                    return;
                }
                *moved = true;
                if let Load::Ready(drawn) = &mut self.load
                    && let Some(card) = drawn.cards.get_mut(*index)
                {
                    card.x = (origin.0 + dx).max(0.);
                    card.y = (origin.1 + dy).max(0.);
                }
                cx.notify();
            }
            Some(Drag::Pan { pointer, offset }) => {
                let delta = event.position - *pointer;
                let max = self.scroll.max_offset();
                let clamp = |value: Pixels, limit: Pixels| value.min(px(0.)).max(-limit);
                self.scroll.set_offset(point(
                    clamp(offset.x + delta.x, max.x),
                    clamp(offset.y + delta.y, max.y),
                ));
                cx.notify();
            }
            None => {}
        }
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(Drag::Card { moved: true, .. }) = self.drag {
            self.just_dragged = true;
        }
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// Put every card back where the layout placed it.
    fn reset_layout(&mut self, cx: &mut Context<Self>) {
        if let Load::Ready(drawn) = &mut self.load {
            for card in &mut drawn.cards {
                (card.x, card.y) = card.home;
            }
        }
        cx.notify();
    }

    /// The toolbar's namespace selection changed: draw the first selected
    /// namespace. Clearing the selection keeps what is drawn, since this page
    /// shows one namespace at a time and "all" has no picture.
    pub fn follow(&mut self, scope: &[SharedString], cx: &mut Context<Self>) {
        if let Some(namespace) = followed(&self.known, scope) {
            self.choose(namespace, cx);
        }
    }

    fn choose(&mut self, namespace: SharedString, cx: &mut Context<Self>) {
        if self.namespace.as_ref() != Some(&namespace) {
            self.namespace = Some(namespace);
            self.refresh(cx);
        }
    }

    fn render_graph(&self, drawn: &Drawn, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border, secondary, primary, background) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.border,
                theme.secondary,
                theme.primary,
                theme.background,
            )
        };
        // Lines first, as plain numbers the paint closure can own.
        let lines: Vec<(f32, f32, f32, f32, bool)> = drawn
            .links
            .iter()
            .filter_map(|&(from, to)| {
                let (a, b) = (drawn.cards.get(from)?, drawn.cards.get(to)?);
                let strong = self.focus.is_some_and(|focus| focus == from || focus == to);
                Some((
                    a.x + CARD_W,
                    a.y + CARD_H / 2.,
                    b.x,
                    b.y + CARD_H / 2.,
                    strong,
                ))
            })
            .collect();
        let faint = muted.opacity(if self.focus.is_some() { 0.25 } else { 0.5 });
        let seconds = self.started.elapsed().as_secs_f32();

        let dot = muted.opacity(0.9);
        let edges = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                for (x1, y1, x2, y2, strong) in lines {
                    let (from, to) = (
                        bounds.origin + point(px(x1), px(y1)),
                        bounds.origin + point(px(x2), px(y2)),
                    );
                    let reach = (x2 - x1).abs() / 2.;
                    let mut path = PathBuilder::stroke(px(if strong { 2. } else { 1.2 }));
                    path.move_to(from);
                    path.cubic_bezier_to(
                        to,
                        bounds.origin + point(px(x1 + reach), px(y1)),
                        bounds.origin + point(px(x2 - reach), px(y2)),
                    );
                    if let Ok(path) = path.build() {
                        window.paint_path(path, if strong { primary } else { faint });
                    }

                    // Dots run from the source to what it feeds.
                    let curve = [(x1, y1), (x1 + reach, y1), (x2 - reach, y2), (x2, y2)];
                    let length = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt() * 1.1;
                    let size_px = if strong { 5. } else { 3.5 };
                    for t in flow(length, seconds) {
                        let (cx_, cy_) = bezier(curve, t);
                        // Fade in and out so a dot does not pop at either end.
                        let fade = (t * std::f32::consts::PI).sin();
                        let color = if strong { primary } else { dot }.opacity(fade);
                        let center = bounds.origin + point(px(cx_), px(cy_));
                        window.paint_quad(
                            fill(
                                Bounds::centered_at(center, size(px(size_px), px(size_px))),
                                color,
                            )
                            .corner_radii(px(size_px / 2.)),
                        );
                    }
                }
            },
        )
        .absolute()
        .size_full();

        // Room for wherever the cards have been dragged to.
        let (width, height) =
            drawn
                .cards
                .iter()
                .fold((drawn.width, drawn.height), |(width, height), card| {
                    (
                        width.max(card.x + CARD_W + PAD),
                        height.max(card.y + CARD_H + PAD),
                    )
                });
        let mut canvas_box = div().relative().w(px(width)).h(px(height)).child(edges);
        for (name, x) in &drawn.headings {
            canvas_box = canvas_box.child(
                div()
                    .absolute()
                    .left(px(*x))
                    .top(px(PAD / 2.))
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(name.to_uppercase()),
            );
        }
        for (index, card) in drawn.cards.iter().enumerate() {
            let color = health_color(&card.health, cx.theme());
            let focused = self.focus == Some(index);
            let target = card.target.clone();
            canvas_box = canvas_box.child(
                v_flex()
                    .id(("topology-card", index))
                    .absolute()
                    .left(px(card.x))
                    .top(px(card.y))
                    .w(px(CARD_W))
                    .h(px(CARD_H))
                    .px_2()
                    .py_1()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused { primary } else { border })
                    .bg(if card.health == "error" || card.health == "warning" {
                        color.opacity(0.14)
                    } else {
                        background
                    })
                    .cursor_pointer()
                    .hover(|card| card.bg(secondary))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.press_card(index, event);
                            // The press belongs to the card, not to the page behind it.
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(card.name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(card.detail.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // The click that ends a drag opens nothing. It arrives before
                        // the page sees the button go up, so the drag may still be live.
                        let dragging = matches!(this.drag, Some(Drag::Card { moved: true, .. }));
                        if std::mem::take(&mut this.just_dragged) || dragging {
                            return;
                        }
                        this.focus = Some(index);
                        if let Some(target) = &target {
                            cx.emit(TopologyEvent::Open(target.clone()));
                        }
                        cx.notify();
                    })),
            );
        }
        div()
            .id("topology-scroll")
            .flex_1()
            .min_h_0()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .when(matches!(self.drag, Some(Drag::Pan { .. })), |page| {
                page.cursor_grabbing()
            })
            .when(!matches!(self.drag, Some(Drag::Pan { .. })), |page| {
                page.cursor_grab()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| this.press_background(event)),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.drag_to(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(cx)),
            )
            .child(canvas_box)
            .into_any_element()
    }
}

impl Render for TopologyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        // Whether any card is away from where the layout put it.
        let moved = matches!(&self.load, Load::Ready(drawn)
            if drawn.cards.iter().any(|card| (card.x, card.y) != card.home));
        let body = match &self.load {
            Load::Failed(reason) => message(reason.clone(), palette.danger).into_any_element(),
            Load::Loading => message("Drawing the graph…", palette.muted).into_any_element(),
            Load::Ready(drawn) if drawn.cards.is_empty() => message(
                "Nothing here: this namespace has no pods, services or ingresses.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready(drawn) => v_flex()
                .flex_1()
                .min_h_0()
                .when(drawn.truncated, |column| {
                    column.child(
                        div()
                            .px_4()
                            .pb_1()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(
                                "This namespace is too large to draw in full; the graph was cut.",
                            ),
                    )
                })
                .child({
                    let (tone, label, detail, progress) = graph_status(drawn);
                    div().px_4().pb_2().child(cards::status_card(
                        tone,
                        label,
                        detail,
                        Some(progress),
                        cx.theme(),
                    ))
                })
                .child(self.render_graph(drawn, cx))
                .into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .child(page_title(IconName::Network, "Topology", cx.theme()))
                    .when_some(self.namespace.clone(), |row, namespace| {
                        row.child(pill(namespace, palette.muted))
                    })
                    .child(div().flex_1())
                    .when(moved, |row| {
                        row.child(
                            button(
                                "topology-reset",
                                "Reset layout",
                                true,
                                Emphasis::Plain,
                                cx.theme(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.reset_layout(cx))),
                        )
                    }),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CARD_H, CARD_W, COLUMN_GAP, Card, DOT_SPACING, Drawn, PAD, ROW_GAP, bezier, flow, followed,
        graph_status, layout,
    };
    use k8s_metrics::{TopologyEdge, TopologyNode};

    fn node(id: &str, kind: &str, name: &str) -> TopologyNode {
        TopologyNode {
            id: id.into(),
            kind: kind.into(),
            sub_kind: None,
            name: name.into(),
            namespace: Some("shop".into()),
            health: "ok".into(),
            detail: None,
            resource: None,
        }
    }

    fn edge(from: &str, to: &str) -> TopologyEdge {
        TopologyEdge {
            from: from.into(),
            to: to.into(),
            kind: "x".into(),
        }
    }

    #[test]
    fn the_graph_status_counts_what_is_healthy_and_names_the_worst() {
        use k8s_app::present::Tone;
        let drawn = |health: &[&str]| Drawn {
            cards: health
                .iter()
                .map(|h| Card {
                    x: 0.,
                    y: 0.,
                    home: (0., 0.),
                    name: "n".into(),
                    detail: "".into(),
                    health: (*h).to_string().into(),
                    target: None,
                })
                .collect(),
            links: vec![(0, 1)],
            headings: Vec::new(),
            width: 0.,
            height: 0.,
            truncated: false,
        };
        let (tone, label, detail, progress) = graph_status(&drawn(&["ok", "ok", "ok"]));
        assert_eq!((tone, label.as_str()), (Tone::Ok, "All healthy"));
        assert_eq!(detail, "3 of 3 objects healthy · 1 link");
        assert_eq!(progress, (3, 3));
        let (tone, label, _, _) = graph_status(&drawn(&["ok", "pending", "unknown"]));
        assert_eq!((tone, label.as_str()), (Tone::Warn, "2 not ready"));
        let (tone, label, _, progress) = graph_status(&drawn(&["ok", "error", "pending"]));
        assert_eq!((tone, label.as_str()), (Tone::Error, "1 unhealthy"));
        assert_eq!(progress, (1, 3));
    }

    #[test]
    fn traffic_runs_left_to_right_and_empty_columns_take_no_room() {
        let nodes = [node("s", "Service", "web"), node("p", "Pod", "web-1")];
        let laid = layout(&nodes, &[edge("s", "p")]);
        let (service, pod) = (laid.positions[0].unwrap(), laid.positions[1].unwrap());
        assert!(service.0 < pod.0, "service is left of its pod");
        assert_eq!(
            pod.0 - service.0,
            CARD_W + COLUMN_GAP,
            "no gap for the absent columns"
        );
        assert_eq!(laid.width, PAD * 2. + 2. * CARD_W + COLUMN_GAP);
        assert_eq!(laid.links, vec![(0, 1)]);
        assert_eq!(
            laid.headings.iter().map(|h| h.0).collect::<Vec<_>>(),
            ["Service", "Pod"]
        );
    }

    #[test]
    fn a_column_follows_the_order_of_what_it_connects_to() {
        // Two services; pods listed in the opposite order to their services.
        let nodes = [
            node("sa", "Service", "a"),
            node("sb", "Service", "b"),
            node("pb", "Pod", "p1"),
            node("pa", "Pod", "p2"),
        ];
        let laid = layout(&nodes, &[edge("sa", "pa"), edge("sb", "pb")]);
        let y = |at: usize| laid.positions[at].unwrap().1;
        assert!(
            y(3) < y(2),
            "the pod of service a sits above the pod of service b"
        );
    }

    #[test]
    fn a_kind_with_no_column_is_left_out_with_its_links() {
        let nodes = [node("p", "Pod", "p"), node("x", "Mystery", "x")];
        let laid = layout(&nodes, &[edge("p", "x")]);
        assert!(laid.positions[1].is_none());
        assert!(laid.links.is_empty());
    }

    #[test]
    fn the_canvas_is_tall_enough_for_the_busiest_column() {
        let nodes = [
            node("a", "Pod", "a"),
            node("b", "Pod", "b"),
            node("c", "Pod", "c"),
        ];
        let laid = layout(&nodes, &[]);
        assert!(laid.height >= PAD * 2. + 3. * (CARD_H + ROW_GAP));
    }

    #[test]
    fn the_toolbar_selection_picks_the_namespace_drawn() {
        let names = |list: &[&str]| -> Vec<gpui_kit::SharedString> {
            list.iter().map(|n| (*n).to_string().into()).collect()
        };
        let known = names(&["default", "shop", "kube-system"]);
        assert_eq!(followed(&known, &names(&["shop"])).as_deref(), Some("shop"));
        assert_eq!(
            followed(&known, &names(&["gone", "kube-system"])).as_deref(),
            Some("kube-system"),
            "a namespace the cluster no longer has is skipped"
        );
        assert!(
            followed(&known, &[]).is_none(),
            "all namespaces keeps the picture"
        );
    }

    #[test]
    fn dots_run_along_a_line_and_longer_lines_carry_more() {
        let at = |length: f32, seconds: f32| flow(length, seconds).collect::<Vec<_>>();
        let short = at(DOT_SPACING * 2., 0.);
        let long = at(DOT_SPACING * 6., 0.);
        assert_eq!((short.len(), long.len()), (2, 6));
        assert!(at(10., 0.).len() == 1, "a short line still carries a dot");

        // They move forward with time and stay on the line.
        let later = at(DOT_SPACING * 6., 0.3);
        assert!(later.iter().all(|t| (0. ..1.).contains(t)));
        assert!(later[0] > long[0], "moved toward the end");
        // After a whole spacing they are back in step with the start.
        let period = DOT_SPACING / super::DOT_SPEED;
        let again = at(DOT_SPACING * 6., period);
        let mut a: Vec<_> = again.iter().map(|t| (t * 1000.).round()).collect();
        let mut b: Vec<_> = long.iter().map(|t| (t * 1000.).round()).collect();
        a.sort_by(f32::total_cmp);
        b.sort_by(f32::total_cmp);
        assert_eq!(a, b);
        assert!(
            at(0., 5.).iter().all(|t| *t == 0.),
            "no length, no movement"
        );
    }

    #[test]
    fn a_curve_starts_and_ends_on_its_ends_and_bends_between() {
        let curve = [(0., 0.), (50., 0.), (50., 100.), (100., 100.)];
        assert_eq!(bezier(curve, 0.), (0., 0.));
        assert_eq!(bezier(curve, 1.), (100., 100.));
        let middle = bezier(curve, 0.5);
        assert!((middle.0 - 50.).abs() < 0.01 && (middle.1 - 50.).abs() < 0.01);
    }
}
