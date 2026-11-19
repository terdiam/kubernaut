//! Usage: a heatmap of every namespace, coloured by how much of what it asked
//! for it actually uses.
//!
//! A request is what the scheduler reserved. A namespace using a tenth of its
//! requests is holding capacity nobody is using; one over them is relying on
//! spare room that may not be there. Both are invisible in a list of pods.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, Theme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, metrics_ops, present::Tone};
use k8s_metrics::NamespaceUsage;
use tokio::runtime::Handle;

use crate::{
    cards::{self, Folds, badge},
    drawer::Target,
    metrics::{bytes, cores},
    namespaces,
    ui::{Palette, message, page_title, pill, segment, segmented_bar},
};

/// Pod usage is sampled every fifteen seconds, so asking more often only
/// repeats the last answer.
const POLL: Duration = Duration::from_secs(10);

pub enum UsageEvent {
    /// Show the namespace's own object, with its Metrics and Quota tabs.
    Open(Target),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Measure {
    Cpu,
    Memory,
}

/// How a namespace's use compares with what it requested.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Heat {
    /// No request is declared, so there is nothing to compare against.
    Unknown,
    /// Under a third of the request: reserved and mostly unused.
    Idle,
    /// A comfortable share.
    Fair,
    /// Close to the request.
    Busy,
    /// Past the request, so it leans on capacity it did not reserve.
    Over,
}

fn heat(usage: f64, request: f64, unset: bool) -> Heat {
    if unset || request <= 0.0 {
        return Heat::Unknown;
    }
    match usage / request {
        ratio if ratio < 1.0 / 3.0 => Heat::Idle,
        ratio if ratio < 0.75 => Heat::Fair,
        ratio if ratio < 1.0 => Heat::Busy,
        _ => Heat::Over,
    }
}

/// Worst first: what a person scanning for trouble wants at the top.
const HEAT_ORDER: [Heat; 5] = [
    Heat::Over,
    Heat::Busy,
    Heat::Fair,
    Heat::Idle,
    Heat::Unknown,
];

/// The id a heat group is folded under.
fn heat_id(heat: Heat) -> &'static str {
    match heat {
        Heat::Over => "usage-over",
        Heat::Busy => "usage-busy",
        Heat::Fair => "usage-fair",
        Heat::Idle => "usage-idle",
        Heat::Unknown => "usage-unknown",
    }
}

/// How the namespaces stand against their requests, in one line.
fn usage_status(tiles: &[Tile]) -> (Tone, String, String) {
    let count = |wanted: Heat| tiles.iter().filter(|tile| tile.heat == wanted).count();
    let (over, busy) = (count(Heat::Over), count(Heat::Busy));
    let (tone, label) = if !tiles.is_empty() && count(Heat::Unknown) == tiles.len() {
        // Nothing declares a request, so "within requests" would claim a
        // comparison that was never made.
        (Tone::Pending, "Nothing to compare".to_string())
    } else if over > 0 {
        (Tone::Error, format!("{over} over request"))
    } else if busy > 0 {
        (Tone::Warn, format!("{busy} near request"))
    } else {
        (Tone::Ok, "Within requests".to_string())
    };
    let detail = format!(
        "of {} namespaces · {} comfortable, {} mostly idle, {} without a request",
        tiles.len(),
        count(Heat::Fair),
        count(Heat::Idle),
        count(Heat::Unknown),
    );
    (tone, label, detail)
}

fn heat_color(heat: Heat, theme: &Theme) -> Hsla {
    match heat {
        Heat::Unknown => theme.muted_foreground,
        Heat::Idle => theme.info,
        Heat::Fair => theme.success,
        Heat::Busy => theme.warning,
        Heat::Over => theme.danger,
    }
}

fn heat_label(heat: Heat) -> &'static str {
    match heat {
        Heat::Unknown => "No request",
        Heat::Idle => "Mostly idle",
        Heat::Fair => "Comfortable",
        Heat::Busy => "Near request",
        Heat::Over => "Over request",
    }
}

/// One tile, with its words built once per refresh.
struct Tile {
    namespace: SharedString,
    heat: Heat,
    /// `64%`, or a dash with no request to compare against.
    percent: SharedString,
    /// What it uses, and against what.
    detail: SharedString,
    pods: SharedString,
    /// For ordering: the most used first.
    weight: f64,
    /// Use as a share of the request, for the bar; never past full.
    fraction: f32,
}

fn tile(entry: &NamespaceUsage, measure: Measure) -> Tile {
    let (usage, request, limit, format): (f64, f64, f64, fn(f64) -> String) = match measure {
        Measure::Cpu => (entry.cpu_usage, entry.cpu_requests, entry.cpu_limits, cores),
        Measure::Memory => (
            entry.memory_usage,
            entry.memory_requests,
            entry.memory_limits,
            bytes,
        ),
    };
    let heat = heat(usage, request, entry.has_unset_requests);
    let percent = if heat == Heat::Unknown {
        "—".to_string()
    } else {
        format!("{:.0}%", usage / request * 100.0)
    };
    let detail = match (request > 0.0, limit > 0.0) {
        (true, true) => format!(
            "{} of {} requested · limit {}",
            format(usage),
            format(request),
            format(limit)
        ),
        (true, false) => format!("{} of {} requested", format(usage), format(request)),
        (false, _) => format!("{} used", format(usage)),
    };
    Tile {
        namespace: entry.namespace.clone().into(),
        heat,
        percent: percent.into(),
        detail: detail.into(),
        pods: match entry.pods {
            1 => "1 pod".to_string(),
            n => format!("{n} pods"),
        }
        .into(),
        weight: usage,
        fraction: if request > 0.0 {
            (usage / request).clamp(0.0, 1.0) as f32
        } else {
            0.0
        },
    }
}

/// Tiles for a measure, the busiest first, with ties in name order so the grid
/// does not shuffle between refreshes.
fn tiles(entries: &[NamespaceUsage], measure: Measure, scope: &[SharedString]) -> Vec<Tile> {
    let mut tiles: Vec<Tile> = entries
        .iter()
        .filter(|entry| namespaces::admits(scope, Some(&entry.namespace)))
        .map(|entry| tile(entry, measure))
        .collect();
    tiles.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.namespace.cmp(&b.namespace))
    });
    tiles
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

pub struct UsageView {
    core: Arc<AppCore>,
    cluster: SharedString,
    scope: Vec<SharedString>,
    measure: Measure,
    load: Load,
    entries: Vec<NamespaceUsage>,
    tiles: Vec<Tile>,
    folds: Folds,
}

impl EventEmitter<UsageEvent> for UsageView {}

impl UsageView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        scope: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            core: Arc::clone(core),
            cluster,
            scope,
            measure: Measure::Cpu,
            load: Load::Loading,
            entries: Vec::new(),
            tiles: Vec::new(),
            folds: Folds::load(core, runtime),
        };
        view.refresh(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        view
    }

    /// Reading the sampler is a lock and a clone, so it needs no task.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        match metrics_ops::namespace_usage(&self.core, &self.cluster) {
            Ok(entries) => {
                self.entries = entries;
                self.load = Load::Ready;
                self.rebuild();
            }
            Err(reason) => self.load = Load::Failed(reason.into()),
        }
        cx.notify();
    }

    fn rebuild(&mut self) {
        self.tiles = tiles(&self.entries, self.measure, &self.scope);
    }

    fn set_measure(&mut self, measure: Measure, cx: &mut Context<Self>) {
        if self.measure != measure {
            self.measure = measure;
            self.rebuild();
            cx.notify();
        }
    }
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let measure = self.measure;
        let toggle = segmented_bar(cx.theme())
            .child(
                segment("usage-cpu", "CPU", measure == Measure::Cpu, cx.theme())
                    .on_click(cx.listener(|this, _, _, cx| this.set_measure(Measure::Cpu, cx))),
            )
            .child(
                segment(
                    "usage-memory",
                    "Memory",
                    measure == Measure::Memory,
                    cx.theme(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_measure(Measure::Memory, cx))),
            );

        let legend = [
            Heat::Idle,
            Heat::Fair,
            Heat::Busy,
            Heat::Over,
            Heat::Unknown,
        ]
        .into_iter()
        .fold(h_flex().gap_3().items_center(), |row, heat| {
            row.child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(heat_color(heat, cx.theme())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(heat_label(heat)),
                    ),
            )
        });

        let body: AnyElement = match &self.load {
            Load::Failed(reason) => message(reason.clone(), palette.danger).into_any_element(),
            Load::Loading => message("Reading usage…", palette.muted).into_any_element(),
            Load::Ready if self.entries.is_empty() => message(
                "Waiting for the first sample. Usage is read every fifteen seconds, and only \
                 while this page is open.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready if self.tiles.is_empty() => message(
                "None of the selected namespaces report usage.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready => {
                let (tone, label, detail) = usage_status(&self.tiles);
                let mut column = v_flex().p_4().gap_3().child(cards::status_card(
                    tone,
                    label,
                    detail,
                    None,
                    cx.theme(),
                ));
                for group_heat in HEAT_ORDER {
                    let group: Vec<(usize, &Tile)> = self
                        .tiles
                        .iter()
                        .enumerate()
                        .filter(|(_, tile)| tile.heat == group_heat)
                        .collect();
                    if group.is_empty() {
                        continue;
                    }
                    let mut grid = h_flex().flex_wrap().gap_3().py_1();
                    for (index, tile) in &group {
                        let color = heat_color(tile.heat, cx.theme());
                        let namespace = tile.namespace.clone();
                        let cluster = self.cluster.clone();
                        grid = grid.child(
                            v_flex()
                                .id(("usage-tile", *index))
                                .w(px(230.))
                                .h(px(112.))
                                .p_3()
                                .gap_1()
                                .rounded_lg()
                                .border_1()
                                .border_color(color.opacity(0.6))
                                .bg(color.opacity(0.14))
                                .cursor_pointer()
                                .hover(|tile| tile.bg(color.opacity(0.24)))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_baseline()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .font_semibold()
                                                .child(tile.namespace.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_semibold()
                                                .text_color(color)
                                                .child(tile.percent.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .h(px(4.))
                                        .w_full()
                                        .rounded_full()
                                        .bg(color.opacity(0.2))
                                        .child(
                                            div()
                                                .h_full()
                                                .rounded_full()
                                                .bg(color)
                                                .w(relative(tile.fraction)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(palette.muted)
                                        .child(tile.detail.clone()),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(palette.muted)
                                        .child(tile.pods.clone()),
                                )
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(UsageEvent::Open(Target {
                                        cluster: cluster.clone(),
                                        resource: "core/v1/namespaces".into(),
                                        kind: "Namespace".into(),
                                        namespace: None,
                                        name: namespace.clone(),
                                    }));
                                })),
                        );
                    }
                    let id = heat_id(group_heat);
                    let folded = self.folds.is_folded(id);
                    let toggle = cx.listener(move |this, _, _, cx| {
                        this.folds.toggle(id);
                        cx.notify();
                    });
                    column = column.child(cards::card(
                        id,
                        heat_label(group_heat),
                        vec![badge(
                            group.len().to_string(),
                            heat_color(group_heat, cx.theme()),
                        )],
                        folded,
                        toggle,
                        vec![grid.into_any_element()],
                        cx.theme(),
                    ));
                }
                div()
                    .id("usage-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(column)
                    .into_any_element()
            }
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
                    .child(page_title(IconName::Gauge, "Usage", cx.theme()))
                    .child(toggle)
                    .when(!self.tiles.is_empty(), |row| {
                        row.child(pill(
                            format!("{} namespaces", self.tiles.len()),
                            palette.muted,
                        ))
                    })
                    .child(div().flex_1())
                    .child(legend),
            )
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .text_xs()
                    .text_color(palette.muted)
                    .child("Use against what each namespace requested, busiest first."),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{Heat, Measure, heat, tiles, usage_status};
    use k8s_app::present::Tone;
    use k8s_metrics::NamespaceUsage;

    fn entry(name: &str, cpu: f64, request: f64, unset: bool) -> NamespaceUsage {
        NamespaceUsage {
            namespace: name.into(),
            pods: 2,
            cpu_usage: cpu,
            cpu_requests: request,
            cpu_limits: 0.0,
            memory_usage: 0.0,
            memory_requests: 0.0,
            memory_limits: 0.0,
            has_unset_requests: unset,
        }
    }

    #[test]
    fn the_status_names_the_worst_namespaces_first() {
        let list = |entries: &[(f64, f64)]| {
            let rows: Vec<NamespaceUsage> = entries
                .iter()
                .enumerate()
                .map(|(i, (usage, request))| entry(&format!("ns{i}"), *usage, *request, false))
                .collect();
            tiles(&rows, Measure::Cpu, &[])
        };
        let none: Vec<NamespaceUsage> = vec![entry("a", 0.5, 0.0, true)];
        let (tone, label, _) = usage_status(&tiles(&none, Measure::Cpu, &[]));
        assert_eq!(
            (tone, label.as_str()),
            (Tone::Pending, "Nothing to compare")
        );
        let (tone, label, _) = usage_status(&list(&[(0.5, 1.0), (0.1, 1.0)]));
        assert_eq!((tone, label.as_str()), (Tone::Ok, "Within requests"));
        let (tone, label, _) = usage_status(&list(&[(0.9, 1.0), (0.1, 1.0)]));
        assert_eq!((tone, label.as_str()), (Tone::Warn, "1 near request"));
        let (tone, label, detail) = usage_status(&list(&[(2.0, 1.0), (0.9, 1.0), (0.1, 1.0)]));
        assert_eq!((tone, label.as_str()), (Tone::Error, "1 over request"));
        assert!(detail.starts_with("of 3 namespaces"), "{detail}");
    }

    #[test]
    fn use_is_coloured_by_how_much_of_the_request_it_takes() {
        assert_eq!(heat(0.1, 1.0, false), Heat::Idle);
        assert_eq!(heat(0.5, 1.0, false), Heat::Fair);
        assert_eq!(heat(0.9, 1.0, false), Heat::Busy);
        assert_eq!(heat(1.0, 1.0, false), Heat::Over);
        assert_eq!(heat(3.0, 1.0, false), Heat::Over);
    }

    #[test]
    fn nothing_to_compare_against_is_unknown_not_idle() {
        assert_eq!(heat(0.5, 0.0, false), Heat::Unknown, "no request declared");
        assert_eq!(
            heat(0.5, 1.0, true),
            Heat::Unknown,
            "some container has none"
        );
    }

    #[test]
    fn the_busiest_namespace_comes_first_and_ties_keep_name_order() {
        let entries = [
            entry("b", 0.5, 1.0, false),
            entry("a", 0.5, 1.0, false),
            entry("busy", 2.0, 1.0, false),
        ];
        let names: Vec<String> = tiles(&entries, Measure::Cpu, &[])
            .iter()
            .map(|t| t.namespace.to_string())
            .collect();
        assert_eq!(names, ["busy", "a", "b"]);
    }

    #[test]
    fn a_namespace_scope_narrows_the_grid() {
        let entries = [entry("a", 1.0, 1.0, false), entry("b", 1.0, 1.0, false)];
        let only_a = tiles(&entries, Measure::Cpu, &["a".into()]);
        assert_eq!(only_a.len(), 1);
        assert_eq!(&*only_a[0].namespace, "a");
    }

    #[test]
    fn a_tile_says_how_much_of_the_request_is_used() {
        let shown = tiles(&[entry("a", 0.25, 0.5, false)], Measure::Cpu, &[]);
        assert_eq!(&*shown[0].percent, "50%");
        assert_eq!(&*shown[0].detail, "250m of 500m requested");
        let unknown = tiles(&[entry("a", 0.25, 0.0, true)], Measure::Cpu, &[]);
        assert_eq!(&*unknown[0].percent, "—");
        assert_eq!(&*unknown[0].detail, "250m used");
    }
}
