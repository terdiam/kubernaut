//! The Metrics tab: CPU and memory over time for a pod, node, namespace or
//! workload, against what it declares, and for a workload what its containers
//! suggest they should ask for.
//!
//! Prometheus gives real history at once; metrics-server only gives the
//! present, so there the chart fills while the tab stays open. The tab says
//! which it is rather than drawing an empty chart without a word.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    base::StyledExt,
    component::{ActiveTheme, chart::AreaChart, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    metrics_ops::{self, SizingReport},
    present,
};
use k8s_metrics::{Confidence, MetricSource, MetricTarget, ObjectMetrics, Recommendation};
use tokio::runtime::Handle;

use crate::{
    drawer::Target,
    row_actions,
    ui::{Palette, message, pill},
};

/// History shown: the last hour.
const WINDOW_MS: i64 = 60 * 60 * 1000;
/// How often the tab asks again. Sampling runs on its own clock; this just
/// keeps the screen near it.
const POLL: Duration = Duration::from_secs(5);
/// Points kept for a workload: an hour at one every five seconds.
const HISTORY: usize = 720;

/// What to chart for a drawer's object, or `None` for a kind with no metrics.
pub fn metric_target(target: &Target) -> Option<MetricTarget> {
    let namespace = target.namespace.as_ref().map(ToString::to_string);
    match (&*target.resource, namespace) {
        ("core/v1/pods", Some(namespace)) => Some(MetricTarget::Pod {
            namespace,
            name: target.name.to_string(),
        }),
        ("core/v1/nodes", _) => Some(MetricTarget::Node {
            name: target.name.to_string(),
        }),
        ("core/v1/namespaces", _) => Some(MetricTarget::Namespace {
            name: target.name.to_string(),
        }),
        (resource, Some(namespace)) if row_actions::is_workload(resource) => {
            Some(MetricTarget::Workload {
                namespace,
                resource: resource.to_string(),
                name: target.name.to_string(),
            })
        }
        _ => None,
    }
}

/// CPU as a person reads it: millicores below one core.
pub fn cores(value: f64) -> String {
    if value <= 0.0 {
        "0".to_string()
    } else if value < 1.0 {
        format!("{:.0}m", value * 1000.0)
    } else {
        format!("{value:.2}")
    }
}

/// Memory as a person reads it: Mi, or Gi from a gibibyte up.
pub fn bytes(value: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if value <= 0.0 {
        "0".to_string()
    } else if value < 1024.0 * MIB {
        format!("{:.0} Mi", value / MIB)
    } else {
        format!("{:.1} Gi", value / (1024.0 * MIB))
    }
}

/// `150m` against a declared `100m` request and `500m` limit, or the words for
/// nothing being declared.
fn declared_line(request: f64, limit: f64, format: fn(f64) -> String) -> String {
    let part = |label: &str, value: f64| {
        if value > 0.0 {
            format!("{label} {}", format(value))
        } else {
            format!("no {label}")
        }
    };
    format!("{} · {}", part("request", request), part("limit", limit))
}

#[derive(Clone)]
struct Pt {
    label: SharedString,
    value: f64,
}

/// A series for the chart, oldest first, labelled by how long ago.
fn series(points: &[k8s_metrics::Point], value: impl Fn(&k8s_metrics::Point) -> f64) -> Vec<Pt> {
    let Some(newest) = points.last().map(|p| p.at) else {
        return Vec::new();
    };
    points
        .iter()
        .map(|point| {
            let mut ago = String::new();
            present::write_age(&mut ago, (newest - point.at) / 1000);
            Pt {
                label: if ago.is_empty() || ago == "0s" {
                    "now".into()
                } else {
                    format!("{ago} ago").into()
                },
                value: value(point),
            }
        })
        .collect()
}

fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Reasonable => "Reasonable",
        Confidence::Indicative => "Indicative",
        Confidence::Insufficient => "Too few samples",
    }
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

pub struct MetricsView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    metric: MetricTarget,
    /// The workload this is, when there is sizing to show.
    workload: Option<(SharedString, SharedString, SharedString)>,
    load: Load,
    metrics: Option<ObjectMetrics>,
    sizing: Option<SizingReport>,
    /// A workload's own history, one point per refresh: the sampler keeps none
    /// for a workload, so this tab does, for as long as it is open.
    workload_points: Vec<k8s_metrics::Point>,
}

impl MetricsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        metric: MetricTarget,
        cx: &mut Context<Self>,
    ) -> Self {
        let workload = match (&metric, &target.namespace) {
            (MetricTarget::Workload { .. }, Some(namespace)) => Some((
                namespace.clone(),
                target.resource.clone(),
                target.name.clone(),
            )),
            _ => None,
        };
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster: target.cluster.clone(),
            metric,
            workload,
            load: Load::Loading,
            metrics: None,
            sizing: None,
            workload_points: Vec::new(),
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

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (core, cluster, metric) = (
            Arc::clone(&self.core),
            self.cluster.clone(),
            self.metric.clone(),
        );
        let workload = self.workload.clone();
        let task = self.runtime.spawn(async move {
            let metrics = metrics_ops::object_metrics(&core, &cluster, &metric, WINDOW_MS).await?;
            let (sizing, point) = match workload {
                Some((namespace, resource, name)) => (
                    metrics_ops::workload_sizing(&core, &cluster, &namespace, &resource, &name)
                        .await
                        .ok(),
                    metrics_ops::workload_point(&core, &cluster, &namespace, &resource, &name)
                        .await
                        .ok()
                        .flatten(),
                ),
                None => (None, None),
            };
            Ok::<_, String>((metrics, sizing, point))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                match result {
                    Ok((mut metrics, sizing, point)) => {
                        if let Some(point) = point {
                            this.workload_points.push(point);
                            let excess = this.workload_points.len().saturating_sub(HISTORY);
                            this.workload_points.drain(..excess);
                        }
                        // Prometheus has its own history; otherwise use what this
                        // tab has gathered.
                        if metrics.points.is_empty() && !this.workload_points.is_empty() {
                            metrics.points.clone_from(&this.workload_points);
                            metrics.source = MetricSource::MetricsServer;
                            metrics.note = None;
                        }
                        this.metrics = Some(metrics);
                        this.sizing = sizing;
                        this.load = Load::Ready;
                    }
                    Err(reason) => this.load = Load::Failed(reason.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_chart(
        &self,
        title: &'static str,
        now: String,
        declared: String,
        data: Vec<Pt>,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (border, muted) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground)
        };
        v_flex()
            .p_3()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(muted)
                            .child(title),
                    )
                    .child(div().text_lg().font_semibold().child(now)),
            )
            .child(div().text_xs().text_color(muted).child(declared))
            .child(
                div().h(px(110.)).child(
                    AreaChart::new(data)
                        .x(|point: &Pt| point.label.clone())
                        .y(|point: &Pt| point.value)
                        .stroke(color)
                        .fill(color.opacity(0.18))
                        .x_axis(false)
                        .grid(false),
                ),
            )
    }

    fn render_sizing(&self, report: &SizingReport, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, secondary, success) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.secondary,
                theme.success,
            )
        };
        let row = |label: &'static str, now: String, suggested: String| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(div().w(px(130.)).flex_none().text_color(muted).child(label))
                .child(div().w(px(90.)).flex_none().child(now))
                .child(div().text_color(success).child(format!("→ {suggested}")))
        };
        let mut column = v_flex().gap_3();
        if let Some(note) = &report.note {
            column = column.child(div().text_sm().text_color(muted).child(note.clone()));
        }
        for rec in &report.recommendations {
            column = column.child(self.render_recommendation(rec, border, muted, secondary, &row));
        }
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child("SIZING SUGGESTIONS"),
            )
            .child(column)
    }

    fn render_recommendation(
        &self,
        rec: &Recommendation,
        border: Hsla,
        muted: Hsla,
        secondary: Hsla,
        row: &dyn Fn(&'static str, String, String) -> Div,
    ) -> impl IntoElement {
        let enough = rec.confidence != Confidence::Insufficient;
        v_flex()
            .p_3()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().font_semibold().child(rec.container.clone()))
                    .child(pill(confidence_label(rec.confidence), muted)),
            )
            .when(enough, |card| {
                card.child(row(
                    "CPU request",
                    cores(rec.current_cpu_request),
                    cores(rec.recommended_cpu_request),
                ))
                .child(row(
                    "Memory request",
                    bytes(rec.current_memory_request),
                    bytes(rec.recommended_memory_request),
                ))
                .child(row(
                    "Memory limit",
                    bytes(rec.current_memory_limit),
                    bytes(rec.recommended_memory_limit),
                ))
                .child(div().text_xs().text_color(muted).child(format!(
                    "Observed: CPU p95 {} (max {}), memory p95 {} (max {})",
                    cores(rec.cpu_p95),
                    cores(rec.cpu_max),
                    bytes(rec.memory_p95),
                    bytes(rec.memory_max),
                )))
            })
            .children(
                rec.notes
                    .iter()
                    .take(if enough { 4 } else { 1 })
                    .map(|note| {
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(secondary)
                            .text_xs()
                            .text_color(muted)
                            .child(note.clone())
                    }),
            )
    }
}

impl Render for MetricsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let (info, warning) = {
            let theme = cx.theme();
            (theme.info, theme.warning)
        };
        let body = match (&self.load, &self.metrics) {
            (Load::Failed(reason), _) => message(reason.clone(), palette.danger).into_any_element(),
            (Load::Loading, _) | (_, None) => {
                message("Reading the metrics…", palette.muted).into_any_element()
            }
            (Load::Ready, Some(metrics)) => {
                let last = metrics.points.last().copied().unwrap_or_default();
                let source = match metrics.source {
                    MetricSource::Prometheus => "From Prometheus, with history.",
                    MetricSource::MetricsServer => {
                        "From metrics-server, which keeps no history: the chart fills while this \
                         tab stays open."
                    }
                    // The note says why, and "no source" would contradict it.
                    MetricSource::None if metrics.note.is_some() => "",
                    MetricSource::None => "No metrics source found in this cluster.",
                };
                let cpu = series(&metrics.points, |p| p.cpu);
                let memory = series(&metrics.points, |p| p.memory / (1024.0 * 1024.0));
                let mut column = v_flex().p_3().gap_3();
                column = column.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .when(!source.is_empty(), |row| {
                            row.child(div().text_xs().text_color(palette.muted).child(source))
                        })
                        .when(metrics.pod_count > 1, |row| {
                            row.child(pill(format!("{} pods", metrics.pod_count), palette.muted))
                        }),
                );
                if let Some(note) = &metrics.note {
                    column = column.child(
                        div()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(note.clone()),
                    );
                }
                column = column
                    .child(self.render_chart(
                        "CPU",
                        cores(last.cpu),
                        declared_line(metrics.cpu_requests, metrics.cpu_limits, cores),
                        cpu,
                        info,
                        cx,
                    ))
                    .child(self.render_chart(
                        "MEMORY",
                        bytes(last.memory),
                        declared_line(metrics.memory_requests, metrics.memory_limits, bytes),
                        memory,
                        warning,
                        cx,
                    ));
                if let Some(report) = &self.sizing {
                    column = column.child(self.render_sizing(report, cx));
                }
                div()
                    .id("metrics-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(column)
                    .into_any_element()
            }
        };
        div().size_full().child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{bytes, cores, declared_line, metric_target, series};
    use crate::drawer::Target;
    use k8s_metrics::{MetricTarget, Point};

    fn target(resource: &str, namespace: Option<&str>) -> Target {
        Target {
            cluster: "c".into(),
            resource: resource.into(),
            kind: "K".into(),
            namespace: namespace.map(Into::into),
            name: "n".into(),
        }
    }

    #[test]
    fn cpu_reads_in_millicores_below_a_core_and_memory_in_mebibytes() {
        assert_eq!(cores(0.0), "0");
        assert_eq!(cores(0.25), "250m");
        assert_eq!(cores(1.5), "1.50");
        assert_eq!(bytes(64.0 * 1024.0 * 1024.0), "64 Mi");
        assert_eq!(bytes(2.0 * 1024.0 * 1024.0 * 1024.0), "2.0 Gi");
        assert_eq!(bytes(0.0), "0");
    }

    #[test]
    fn what_a_pod_declares_is_said_in_words_and_nothing_declared_is_said_too() {
        assert_eq!(declared_line(0.1, 0.5, cores), "request 100m · limit 500m");
        assert_eq!(declared_line(0.0, 0.0, cores), "no request · no limit");
    }

    #[test]
    fn only_objects_with_pods_or_usage_behind_them_get_metrics() {
        assert_eq!(
            metric_target(&target("core/v1/pods", Some("shop"))),
            Some(MetricTarget::Pod {
                namespace: "shop".into(),
                name: "n".into()
            })
        );
        assert!(matches!(
            metric_target(&target("core/v1/nodes", None)),
            Some(MetricTarget::Node { .. })
        ));
        assert!(matches!(
            metric_target(&target("apps/v1/deployments", Some("shop"))),
            Some(MetricTarget::Workload { .. })
        ));
        assert!(metric_target(&target("core/v1/configmaps", Some("shop"))).is_none());
        assert!(metric_target(&target("core/v1/pods", None)).is_none());
    }

    #[test]
    fn a_series_is_labelled_by_how_long_before_the_newest_point() {
        let point = |at: i64, cpu: f64| Point {
            at,
            cpu,
            ..Default::default()
        };
        let data = series(&[point(0, 1.0), point(60_000, 2.0)], |p| p.cpu);
        assert_eq!(&*data[0].label, "1m0s ago");
        assert_eq!(&*data[1].label, "now");
        assert!(series(&[], |p| p.cpu).is_empty());
    }
}
