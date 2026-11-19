//! The cluster overview: how full it is, whether its nodes are healthy, and
//! what needs attention, with each problem one click from the object.
//!
//! The numbers come from the shared metrics sampler, which keeps its own watch
//! of nodes and pods. This view only reads the latest sample, so it costs a
//! lock and a comparison, not an API call.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, Icon, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, present::Tone};
use k8s_metrics::{
    ClusterSampler,
    model::{ClusterOverview, Issue, NodeScope, ResourceGauge, Severity},
};
use tokio::runtime::Handle;

use crate::{
    cards::{self, Folds, badge},
    drawer::Target,
    ui::{Palette, message, pill, segment, segmented_bar, tone_color},
};

/// How often the latest sample is looked at. The sampler refreshes on its own
/// schedule; this only has to be quick enough to feel live.
const POLL: Duration = Duration::from_secs(2);
/// Issues listed before the rest are summarised, so a failing cluster does not
/// build thousands of rows.
const MAX_ISSUES: usize = 40;

pub enum OverviewEvent {
    Open(Target),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Cores,
    Bytes,
    Count,
}

/// A gauge as the card draws it: the words and fractions, decided once per
/// sample rather than every frame.
#[derive(Debug, PartialEq)]
struct Gauge {
    title: &'static str,
    headline: String,
    detail: String,
    /// 0 to 1, for the bar. The headline keeps the true figure when over.
    used: f32,
    /// Where requests sit on the bar, when usage is what the bar shows.
    requested: Option<f32>,
    tone: Tone,
    note: Option<&'static str>,
}

fn format_amount(value: f64, unit: Unit) -> String {
    match unit {
        Unit::Cores => {
            if value >= 10.0 {
                format!("{value:.0}")
            } else {
                format!("{value:.1}")
            }
        }
        Unit::Bytes => {
            const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
            const MIB: f64 = 1024.0 * 1024.0;
            if value >= GIB {
                format!("{:.1} GiB", value / GIB)
            } else {
                format!("{:.0} MiB", value / MIB)
            }
        }
        Unit::Count => format!("{value:.0}"),
    }
}

fn unit_word(unit: Unit) -> &'static str {
    match unit {
        Unit::Cores => " cores",
        Unit::Bytes => "",
        Unit::Count => " pods",
    }
}

/// Reading a gauge: usage when the cluster reports it, otherwise what is
/// requested, and say which. A confident zero for "unknown" would be worse than
/// showing less.
fn gauge(title: &'static str, g: &ResourceGauge, unit: Unit) -> Gauge {
    let total = if g.allocatable > 0.0 {
        g.allocatable
    } else {
        g.capacity
    };
    if total <= 0.0 {
        return Gauge {
            title,
            headline: "—".into(),
            detail: "No capacity reported".into(),
            used: 0.0,
            requested: None,
            tone: Tone::Ok,
            note: None,
        };
    }

    let (measure, verb) = if g.usage_available {
        (g.usage, "in use")
    } else {
        (g.requests, "requested")
    };
    let ratio = measure / total;
    let tone = if ratio >= 0.95 {
        Tone::Error
    } else if ratio >= 0.8 {
        Tone::Warn
    } else {
        Tone::Ok
    };
    Gauge {
        title,
        headline: format!("{:.0}%", ratio * 100.0),
        detail: format!(
            "{} of {}{} {verb}",
            format_amount(measure, unit),
            format_amount(total, unit),
            unit_word(unit),
        ),
        used: ratio.clamp(0.0, 1.0) as f32,
        requested: (g.usage_available && g.requests > 0.0)
            .then(|| (g.requests / total).clamp(0.0, 1.0) as f32),
        tone,
        note: (!g.usage_available && unit != Unit::Count)
            .then_some("Showing requests. Live usage needs metrics-server."),
    }
}

struct IssueLine {
    tone: Tone,
    title: SharedString,
    message: SharedString,
    target: Target,
}

/// What the page shows for one sample.
struct Shown {
    sampled_at: String,
    gauges: [Gauge; 3],
    nodes_headline: SharedString,
    nodes_detail: SharedString,
    nodes_ok: bool,
    unschedulable: usize,
    issues: Vec<IssueLine>,
    hidden_issues: usize,
    metrics_error: Option<SharedString>,
    /// How the cluster is doing, for the card at the top.
    status: Status,
}

/// The cluster in one line: a state and what it rests on.
struct Status {
    tone: Tone,
    label: &'static str,
    detail: SharedString,
}

/// Errors or an unready node need attention; warnings or a gauge that is
/// filling up deserve a look; otherwise the cluster is healthy.
fn status(
    nodes_ready: usize,
    nodes_total: usize,
    errors: usize,
    warnings: usize,
    fullest: Tone,
) -> Status {
    let (tone, label) = if nodes_ready < nodes_total || errors > 0 {
        (Tone::Error, "Needs attention")
    } else if warnings > 0 || fullest >= Tone::Warn {
        (Tone::Warn, "Worth a look")
    } else {
        (Tone::Ok, "Healthy")
    };
    let count = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let problems = match (errors, warnings) {
        (0, 0) => "nothing needs attention".to_string(),
        (e, 0) => count(e, "error"),
        (0, w) => count(w, "warning"),
        (e, w) => format!("{}, {}", count(e, "error"), count(w, "warning")),
    };
    Status {
        tone,
        label,
        detail: format!("{nodes_ready} of {nodes_total} nodes ready · {problems}").into(),
    }
}

fn issue_line(cluster: &SharedString, issue: &Issue) -> IssueLine {
    let title: SharedString = match &issue.namespace {
        Some(namespace) => format!("{} {namespace}/{}", issue.kind, issue.name),
        None => format!("{} {}", issue.kind, issue.name),
    }
    .into();
    IssueLine {
        tone: match issue.severity {
            Severity::Error => Tone::Error,
            Severity::Warning => Tone::Warn,
        },
        title,
        message: issue.message.as_str().into(),
        target: Target {
            cluster: cluster.clone(),
            resource: issue.resource.as_str().into(),
            kind: issue.kind.as_str().into(),
            namespace: issue.namespace.as_deref().map(Into::into),
            name: issue.name.as_str().into(),
        },
    }
}

fn shown(cluster: &SharedString, overview: &ClusterOverview) -> Shown {
    let nodes = &overview.nodes;
    let mut issues: Vec<&Issue> = overview.issues.iter().collect();
    // Errors before warnings; the sampler's own order is kept within each.
    issues.sort_by_key(|issue| issue.severity == Severity::Warning);
    let hidden_issues = issues.len().saturating_sub(MAX_ISSUES);
    let errors = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .count();
    let warnings = issues.len() - errors;
    let gauges = [
        gauge("CPU", &overview.cpu, Unit::Cores),
        gauge("Memory", &overview.memory, Unit::Bytes),
        gauge("Pods", &overview.pods, Unit::Count),
    ];
    let fullest = gauges.iter().map(|g| g.tone).max().unwrap_or(Tone::Ok);

    Shown {
        sampled_at: overview.sampled_at.clone(),
        status: status(nodes.ready, nodes.total, errors, warnings, fullest),
        gauges,
        nodes_headline: format!("{} of {}", nodes.ready, nodes.total).into(),
        nodes_detail: if nodes.total == 1 {
            "node ready".into()
        } else {
            "nodes ready".into()
        },
        nodes_ok: nodes.not_ready == 0 && nodes.total > 0,
        unschedulable: nodes.unschedulable,
        issues: issues
            .into_iter()
            .take(MAX_ISSUES)
            .map(|issue| issue_line(cluster, issue))
            .collect(),
        hidden_issues,
        metrics_error: overview.metrics_error.as_deref().map(Into::into),
    }
}

pub struct OverviewView {
    cluster: SharedString,
    sampler: Option<Arc<ClusterSampler>>,
    scope: NodeScope,
    shown: Option<Shown>,
    folds: Folds,
}

impl EventEmitter<OverviewEvent> for OverviewView {}

impl OverviewView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        // Starting the sampler spawns tasks, so it has to happen on the runtime.
        let (task_core, task_cluster) = (Arc::clone(core), cluster.clone());
        let started = runtime.spawn(async move {
            let handle = task_core.clusters.require(&task_cluster).ok()?;
            Some(task_core.metrics.ensure(&handle))
        });
        cx.spawn(async move |this, cx| {
            let sampler = started.await.ok().flatten();
            this.update(cx, |this, cx| {
                this.sampler = sampler;
                this.poll(cx);
            })
            .ok();
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            cluster,
            sampler: None,
            scope: NodeScope::All,
            shown: None,
            folds: Folds::load(core, runtime),
        }
    }

    /// Take the latest sample, and redraw only if it is a new one.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let Some(overview) = self.sampler.as_ref().and_then(|s| s.overview(self.scope)) else {
            return;
        };
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.sampled_at == overview.sampled_at)
        {
            return;
        }
        self.shown = Some(shown(&self.cluster, &overview));
        cx.notify();
    }

    fn set_scope(&mut self, scope: NodeScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.shown = None;
            self.poll(cx);
            cx.notify();
        }
    }

    fn card(&self, cx: &App) -> Div {
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .min_w(px(210.))
            .gap_1()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
    }

    fn render_gauge(&self, gauge: &Gauge, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (track, muted, primary) = (theme.border, theme.muted_foreground, theme.primary);
        let fill = match gauge.tone {
            Tone::Ok | Tone::Pending => primary,
            other => tone_color(other, theme),
        };
        let marker = theme.foreground.opacity(0.7);

        self.card(cx)
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(gauge.title),
            )
            .child(
                div()
                    .text_size(px(28.))
                    .font_semibold()
                    .child(gauge.headline.clone()),
            )
            .child(
                div()
                    .relative()
                    .h(px(6.))
                    .w_full()
                    .my_1()
                    .rounded_full()
                    .bg(track)
                    .child(
                        div()
                            .h_full()
                            .w(relative(gauge.used))
                            .rounded_full()
                            .bg(fill),
                    )
                    // Requests as a tick on the track: how much is promised,
                    // next to how much is used.
                    .when_some(gauge.requested, |bar, requested| {
                        bar.child(
                            div()
                                .absolute()
                                .top(px(-2.))
                                .left(relative(requested))
                                .w(px(2.))
                                .h(px(10.))
                                .rounded_full()
                                .bg(marker),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(gauge.detail.clone()),
            )
            .when_some(gauge.note, |card, note| {
                card.child(div().text_xs().text_color(muted).child(note))
            })
    }

    fn render_nodes(&self, shown: &Shown, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, ok, warn, danger) = (
            theme.muted_foreground,
            theme.success,
            theme.warning,
            theme.danger,
        );
        self.card(cx)
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child("Nodes"),
            )
            .child(
                div()
                    .text_size(px(28.))
                    .font_semibold()
                    .child(shown.nodes_headline.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(shown.nodes_detail.clone()),
            )
            .child(
                h_flex()
                    .mt_1()
                    .gap_2()
                    .flex_wrap()
                    .child(if shown.nodes_ok {
                        pill("Healthy", ok)
                    } else {
                        pill("Needs attention", danger)
                    })
                    .when(shown.unschedulable > 0, |row| {
                        row.child(pill(format!("{} cordoned", shown.unschedulable), warn))
                    }),
            )
    }

    fn render_issues(&self, shown: &Shown, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, success, warning) = (theme.muted_foreground, theme.success, theme.warning);

        let rows: Vec<AnyElement> = if shown.issues.is_empty() {
            vec![
                h_flex()
                    .gap_2()
                    .py_2()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .size_4()
                            .text_color(success),
                    )
                    .child("All clear. Nothing needs attention right now.")
                    .into_any_element(),
            ]
        } else {
            let mut rows: Vec<AnyElement> = shown
                .issues
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let target = line.target.clone();
                    let color = tone_color(line.tone, cx.theme());
                    let hover = cx.theme().secondary_hover;
                    h_flex()
                        .id(("issue", index))
                        .gap_3()
                        .items_center()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|row| row.bg(hover))
                        .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(line.title.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .truncate()
                                        .child(line.message.clone()),
                                ),
                        )
                        .child(Icon::new(IconName::ChevronRight).size_4().text_color(muted))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(OverviewEvent::Open(target.clone()));
                        }))
                        .into_any_element()
                })
                .collect();
            if shown.hidden_issues > 0 {
                rows.push(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("and {} more", shown.hidden_issues))
                        .into_any_element(),
                );
            }
            rows
        };

        let id = "overview-attention";
        let folded = self.folds.is_folded(id);
        let toggle = cx.listener(move |this, _, _, cx| {
            this.folds.toggle(id);
            cx.notify();
        });
        let badges = if shown.issues.is_empty() {
            Vec::new()
        } else {
            vec![badge(
                (shown.issues.len() + shown.hidden_issues).to_string(),
                warning,
            )]
        };
        cards::card(
            id,
            "Needs attention",
            badges,
            folded,
            toggle,
            rows,
            cx.theme(),
        )
    }
}

impl Render for OverviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let Some(shown) = self.shown.as_ref() else {
            return message("Reading the cluster…", palette.muted).into_any_element();
        };

        let scopes = [
            (NodeScope::All, "All"),
            (NodeScope::ControlPlane, "Control plane"),
            (NodeScope::Workers, "Workers"),
        ];
        let mut switcher = segmented_bar(cx.theme());
        for (index, (scope, label)) in scopes.into_iter().enumerate() {
            switcher = switcher.child(
                segment(("scope", index), label, self.scope == scope, cx.theme())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_scope(scope, cx))),
            );
        }

        v_flex()
            .id("overview")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .gap_5()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().text_xl().font_semibold().child("Overview"))
                    .child(div().flex_1())
                    .child(switcher),
            )
            .child(cards::status_card(
                shown.status.tone,
                shown.status.label,
                shown.status.detail.clone(),
                None,
                cx.theme(),
            ))
            .when_some(shown.metrics_error.clone(), |page, error| {
                page.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted)
                        .child(format!("Live usage is unavailable: {error}")),
                )
            })
            .child(
                h_flex()
                    .gap_4()
                    .flex_wrap()
                    .items_stretch()
                    .child(self.render_gauge(&shown.gauges[0], cx))
                    .child(self.render_gauge(&shown.gauges[1], cx))
                    .child(self.render_gauge(&shown.gauges[2], cx))
                    .child(self.render_nodes(shown, cx)),
            )
            .child(self.render_issues(shown, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Unit, format_amount, gauge};
    use k8s_app::present::Tone;
    use k8s_metrics::model::ResourceGauge;

    fn resource(
        usage: f64,
        requests: f64,
        allocatable: f64,
        usage_available: bool,
    ) -> ResourceGauge {
        ResourceGauge {
            usage,
            requests,
            limits: 0.0,
            allocatable,
            capacity: allocatable,
            usage_available,
        }
    }

    #[test]
    fn the_cluster_status_follows_nodes_then_errors_then_warnings_then_fullness() {
        use super::status;
        let s = status(3, 3, 0, 0, Tone::Ok);
        assert_eq!((s.tone, s.label), (Tone::Ok, "Healthy"));
        assert_eq!(&*s.detail, "3 of 3 nodes ready · nothing needs attention");
        let s = status(3, 3, 0, 2, Tone::Ok);
        assert_eq!((s.tone, s.label), (Tone::Warn, "Worth a look"));
        assert_eq!(&*s.detail, "3 of 3 nodes ready · 2 warnings");
        let s = status(3, 3, 0, 0, Tone::Warn);
        assert_eq!(s.tone, Tone::Warn, "a nearly full gauge deserves a look");
        let s = status(2, 3, 0, 0, Tone::Ok);
        assert_eq!((s.tone, s.label), (Tone::Error, "Needs attention"));
        let s = status(3, 3, 1, 4, Tone::Ok);
        assert_eq!(s.tone, Tone::Error);
        assert_eq!(&*s.detail, "3 of 3 nodes ready · 1 error, 4 warnings");
    }

    #[test]
    fn amounts_read_naturally() {
        assert_eq!(format_amount(3.24, Unit::Cores), "3.2");
        assert_eq!(format_amount(16.0, Unit::Cores), "16");
        assert_eq!(
            format_amount(8.0 * 1024.0 * 1024.0 * 1024.0, Unit::Bytes),
            "8.0 GiB"
        );
        assert_eq!(
            format_amount(512.0 * 1024.0 * 1024.0, Unit::Bytes),
            "512 MiB"
        );
        assert_eq!(format_amount(47.0, Unit::Count), "47");
    }

    #[test]
    fn a_gauge_shows_usage_against_what_the_scheduler_may_hand_out() {
        let g = gauge("CPU", &resource(2.0, 4.0, 8.0, true), Unit::Cores);
        assert_eq!(g.headline, "25%");
        assert_eq!(g.detail, "2.0 of 8.0 cores in use");
        assert!((g.used - 0.25).abs() < 1e-6);
        assert_eq!(g.requested, Some(0.5), "requests are drawn as a tick");
        assert_eq!(g.tone, Tone::Ok);
        assert!(g.note.is_none());
    }

    #[test]
    fn without_metrics_the_gauge_falls_back_to_requests_and_says_so() {
        let g = gauge("CPU", &resource(0.0, 4.0, 8.0, false), Unit::Cores);
        assert_eq!(g.headline, "50%");
        assert!(g.detail.ends_with("requested"));
        assert_eq!(g.requested, None, "the bar already shows requests");
        assert!(g.note.is_some());
    }

    #[test]
    fn a_nearly_full_gauge_changes_tone_and_the_bar_never_overflows() {
        let warn = gauge("Pods", &resource(90.0, 0.0, 110.0, true), Unit::Count);
        assert_eq!(warn.tone, Tone::Warn);
        let full = gauge("Pods", &resource(130.0, 0.0, 110.0, true), Unit::Count);
        assert_eq!(full.tone, Tone::Error);
        assert_eq!(full.headline, "118%", "the figure stays honest");
        assert!((full.used - 1.0).abs() < f32::EPSILON, "the bar is clamped");
        assert!(full.note.is_none(), "pod counts need no metrics-server");
    }

    #[test]
    fn a_cluster_that_reports_no_capacity_says_so_instead_of_dividing_by_zero() {
        let g = gauge("Memory", &resource(0.0, 0.0, 0.0, true), Unit::Bytes);
        assert_eq!(g.headline, "—");
        assert_eq!(g.used, 0.0);
    }
}
