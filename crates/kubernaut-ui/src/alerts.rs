//! Alerts: what Alertmanager is firing, and a way to quiet it.
//!
//! Firing alerts come first, worst severity at the top; silenced ones follow,
//! dimmed. Selecting one shows its labels and annotations and offers the two
//! things people do with it: silence it for a while, or end a silence early.

use std::{ops::Range, sync::Arc, time::Duration};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, alert_ops, present, present::Tone};
use k8s_metrics::{Alert, AlertmanagerTarget};
use tokio::runtime::Handle;

use crate::{
    cards::{self, Folds, badge},
    drawer::Target,
    namespaces,
    rows::contains_ignore_case,
    ui::{
        Emphasis, Palette, button_icon, message, page_title, pill, segment, segmented_bar,
        toast_error, toast_success, tone_color,
    },
};

const ROW_HEIGHT: Pixels = px(32.);
/// Alerts resolve and fire on the scale of minutes; this keeps the page close
/// to Alertmanager without a request per second.
const POLL: Duration = Duration::from_secs(15);
/// How long a silence can last, as the buttons offer them.
const DURATIONS: [(u64, &str); 3] = [(1, "1 hour"), (4, "4 hours"), (24, "1 day")];

pub enum AlertsEvent {
    /// Show the object's YAML and events.
    Open(Target),
}

/// Rank for sorting: lower is more urgent.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "error" | "high" => 1,
        "warning" => 2,
        _ => 3,
    }
}

fn tone_of(severity: &str) -> Option<Tone> {
    match severity_rank(severity) {
        0 | 1 => Some(Tone::Error),
        2 => Some(Tone::Warn),
        _ => None,
    }
}

/// The object an alert is about, when its labels name one the app can open.
fn subject_of(
    labels: &[(SharedString, SharedString)],
) -> Option<(&'static str, &'static str, SharedString)> {
    const KINDS: [(&str, &str, &str); 5] = [
        ("pod", "core/v1/pods", "Pod"),
        ("deployment", "apps/v1/deployments", "Deployment"),
        ("statefulset", "apps/v1/statefulsets", "StatefulSet"),
        ("daemonset", "apps/v1/daemonsets", "DaemonSet"),
        ("node", "core/v1/nodes", "Node"),
    ];
    KINDS.iter().find_map(|(label, resource, kind)| {
        labels
            .iter()
            .find(|(key, _)| key == label)
            .map(|(_, value)| (*resource, *kind, value.clone()))
    })
}

/// One alert as the list draws it. Labels are built once, at load.
struct Line {
    fingerprint: SharedString,
    name: SharedString,
    severity: SharedString,
    namespace: Option<SharedString>,
    summary: SharedString,
    silenced: bool,
    silences: Vec<String>,
    age: SharedString,
    labels: Vec<(SharedString, SharedString)>,
    annotations: Vec<(SharedString, SharedString)>,
}

fn pairs(map: std::collections::BTreeMap<String, String>) -> Vec<(SharedString, SharedString)> {
    map.into_iter().map(|(k, v)| (k.into(), v.into())).collect()
}

fn line(alert: Alert, now: i64) -> Line {
    let mut age = String::new();
    if let Some(at) = present::parse_timestamp(&alert.starts_at) {
        present::write_age(&mut age, now - at);
    }
    let label = |key: &str| alert.labels.get(key).cloned();
    let summary = alert
        .annotations
        .get("summary")
        .or_else(|| alert.annotations.get("description"))
        .or_else(|| alert.annotations.get("message"))
        .cloned()
        .unwrap_or_default();
    Line {
        name: label("alertname")
            .unwrap_or_else(|| "(unnamed)".into())
            .into(),
        severity: label("severity").unwrap_or_default().into(),
        namespace: label("namespace").map(Into::into),
        summary: summary.into(),
        silenced: alert.state == "suppressed" || !alert.silenced_by.is_empty(),
        silences: alert.silenced_by,
        age: age.into(),
        fingerprint: alert.fingerprint.into(),
        labels: pairs(alert.labels),
        annotations: pairs(alert.annotations),
    }
}

/// Firing alerts before silenced ones, then by urgency, then by name.
fn order(lines: &mut [Line]) {
    lines.sort_by(|a, b| {
        (
            a.silenced,
            severity_rank(&a.severity),
            &a.name,
            &a.fingerprint,
        )
            .cmp(&(
                b.silenced,
                severity_rank(&b.severity),
                &b.name,
                &b.fingerprint,
            ))
    });
}

/// Indices of the lines a filter keeps.
fn matching(lines: &[Line], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            [
                &*line.name,
                &*line.severity,
                &*line.summary,
                line.namespace.as_deref().unwrap_or_default(),
            ]
            .into_iter()
            .any(|field| contains_ignore_case(field, needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// How the alerts stand, for the card above the list: the worst that is still
/// firing, and what is silenced. Nothing firing is good news worth saying.
fn alerts_status(lines: &[Line]) -> (Tone, String, String) {
    let firing: Vec<&Line> = lines.iter().filter(|line| !line.silenced).collect();
    let silenced = lines.len() - firing.len();
    let silenced_note = if silenced > 0 {
        format!("{silenced} silenced")
    } else {
        "none silenced".to_string()
    };
    if firing.is_empty() {
        let label = if silenced > 0 {
            "All silenced"
        } else {
            "Nothing firing"
        };
        let tone = if silenced > 0 {
            Tone::Pending
        } else {
            Tone::Ok
        };
        return (tone, label.to_string(), silenced_note);
    }
    let worst = firing
        .iter()
        .filter_map(|line| tone_of(&line.severity))
        .max()
        .unwrap_or(Tone::Warn);
    let urgent = firing
        .iter()
        .filter(|line| severity_rank(&line.severity) <= 1)
        .count();
    let detail = if urgent > 0 {
        format!("{urgent} urgent · {silenced_note}")
    } else {
        silenced_note
    };
    (worst, format!("{} firing", firing.len()), detail)
}

enum Load {
    Loading,
    Ready,
    /// The cluster has no Alertmanager. Normal, so not an error.
    Absent,
    Failed(SharedString),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Silence,
    Expire,
}

pub struct AlertsView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    scope: Vec<SharedString>,
    load: Load,
    target: Option<AlertmanagerTarget>,
    lines: Vec<Line>,
    shown: Vec<usize>,
    filter: Entity<InputState>,
    /// Identifies the row across refreshes, which re-sort the list.
    selected: Option<SharedString>,
    /// Index into [`DURATIONS`] for the next silence.
    duration: usize,
    busy: bool,
    scroll: UniformListScrollHandle,
    _filter: Subscription,
    folds: Folds,
}

impl EventEmitter<AlertsEvent> for AlertsView {}

impl AlertsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        scope: Vec<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subscription =
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_filter(cx);
                }
            });
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            scope,
            load: Load::Loading,
            target: None,
            lines: Vec::new(),
            shown: Vec::new(),
            filter,
            selected: None,
            duration: 1,
            busy: false,
            scroll: UniformListScrollHandle::new(),
            _filter: subscription,
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

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (core, cluster, known) = (
            Arc::clone(&self.core),
            self.cluster.clone(),
            self.target.clone(),
        );
        let task = self
            .runtime
            .spawn(async move { alert_ops::survey(&core, &cluster, known).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_refresh(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        result: Result<Option<alert_ops::AlertSurvey>, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(survey)) => {
                let now = present::now();
                self.lines = survey
                    .alerts
                    .into_iter()
                    .map(|alert| line(alert, now))
                    .filter(|line| namespaces::admits(&self.scope, line.namespace.as_deref()))
                    .collect();
                order(&mut self.lines);
                self.target = Some(survey.target);
                self.load = Load::Ready;
                self.apply_filter(cx);
            }
            Ok(None) => {
                self.target = None;
                self.lines.clear();
                self.shown.clear();
                self.load = Load::Absent;
                cx.notify();
            }
            Err(message) => {
                self.load = Load::Failed(message.into());
                cx.notify();
            }
        }
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.read(cx).value();
        self.shown = matching(&self.lines, needle.trim());
        cx.notify();
    }

    fn selected_line(&self) -> Option<&Line> {
        let key = self.selected.as_ref()?;
        self.lines.iter().find(|line| &line.fingerprint == key)
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.selected_line() else {
            return;
        };
        let (title, detail, ok): (SharedString, SharedString, &str) = match action {
            Action::Silence => (
                format!("Silence {} for {}?", line.name, DURATIONS[self.duration].1).into(),
                "Every alert with this name, in every namespace, stops notifying until the time is up."
                    .into(),
                "Silence",
            ),
            Action::Expire => (
                format!("Stop silencing {}?", line.name).into(),
                "It notifies again if it is still firing.".into(),
                "Stop silencing",
            ),
        };
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog
                .title(title.clone())
                .description(detail.clone())
                .ok_text(ok)
                .ok_variant(ButtonVariant::Primary)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| this.run(action, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(line), Some(target)) = (self.selected_line(), self.target.clone()) else {
            return;
        };
        let (name, silences) = (line.name.to_string(), line.silences.clone());
        let duration = Duration::from_secs(DURATIONS[self.duration].0 * 3600);
        self.busy = true;
        cx.notify();

        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let label = name.clone();
        let task = self.runtime.spawn(async move {
            match action {
                Action::Silence => alert_ops::silence(&core, &cluster, &target, &name, duration)
                    .await
                    .map(|_| ()),
                Action::Expire => alert_ops::expire(&core, &cluster, &target, &name, &silences)
                    .await
                    .map(|_| ()),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        toast_success(
                            window,
                            cx,
                            match action {
                                Action::Silence => format!("Silenced {label}"),
                                Action::Expire => format!("{label} notifies again"),
                            },
                        );
                        this.refresh(cx);
                    }
                    Err(reason) => toast_error(
                        window,
                        cx,
                        match action {
                            Action::Silence => format!("Could not silence {label}"),
                            Action::Expire => format!("Could not end the silence on {label}"),
                        },
                        reason,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_detail(&self, line: &Line, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, border, muted, secondary) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.secondary,
        );
        let color = tone_of(&line.severity).map_or(muted, |tone| tone_color(tone, theme));
        let idle = !self.busy;
        let subject = subject_of(&line.labels);
        let target = subject.as_ref().map(|(resource, kind, name)| Target {
            cluster: self.cluster.clone(),
            resource: (*resource).into(),
            kind: (*kind).into(),
            namespace: line.namespace.clone(),
            name: name.clone(),
        });
        let silence = cx.listener(|this, _, window, cx| this.confirm(Action::Silence, window, cx));
        let expire = cx.listener(|this, _, window, cx| this.confirm(Action::Expire, window, cx));
        let open = target.map(|target| {
            cx.listener(move |_, _, _, cx| cx.emit(AlertsEvent::Open(target.clone())))
        });
        let card = |id: &'static str,
                    title: &'static str,
                    count: usize,
                    rows: Vec<AnyElement>,
                    cx: &mut Context<Self>| {
            let folded = self.folds.is_folded(id);
            let toggle = cx.listener(move |this, _, _, cx| {
                this.folds.toggle(id);
                cx.notify();
            });
            cards::card(
                id,
                title,
                vec![badge(count.to_string(), muted)],
                folded,
                toggle,
                rows,
                cx.theme(),
            )
        };
        let fields = |pairs: &[(SharedString, SharedString)], cx: &mut Context<Self>| {
            pairs
                .iter()
                .map(|(key, value)| cards::field("alert", key, value, 120., cx.theme()))
                .collect::<Vec<_>>()
        };
        let mut cards_column = v_flex().gap_3();
        cards_column = cards_column.child(card(
            "alerts-labels",
            "Labels",
            line.labels.len(),
            fields(&line.labels, cx),
            cx,
        ));
        if !line.annotations.is_empty() {
            cards_column = cards_column.child(card(
                "alerts-annotations",
                "Annotations",
                line.annotations.len(),
                fields(&line.annotations, cx),
                cx,
            ));
        }

        v_flex()
            .w(px(460.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(border)
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(div().font_semibold().child(line.name.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when(!line.severity.is_empty(), |row| {
                                row.child(pill(line.severity.clone(), color))
                            })
                            .child(pill(
                                if line.silenced { "Silenced" } else { "Firing" },
                                if line.silenced {
                                    muted
                                } else {
                                    cx.theme().danger
                                },
                            ))
                            .when(!line.age.is_empty(), |row| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("for {}", line.age)),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("alerts-detail")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_3()
                    .when(!line.summary.is_empty(), |column| {
                        column.child(
                            div()
                                .p_2()
                                .rounded_md()
                                .text_sm()
                                .bg(secondary)
                                .child(line.summary.clone()),
                        )
                    })
                    .child(cards_column),
            )
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(border)
                    .when(!line.silenced, |column| {
                        column.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().text_xs().text_color(muted).child("Silence for"))
                                .child(DURATIONS.iter().enumerate().fold(
                                    segmented_bar(cx.theme()),
                                    |bar, (index, (_, label))| {
                                        bar.child(
                                            segment(
                                                ("alerts-duration", index),
                                                *label,
                                                self.duration == index,
                                                cx.theme(),
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.duration = index;
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                    },
                                )),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(if line.silenced {
                                button_icon(
                                    "alerts-expire",
                                    IconName::Bell,
                                    "Stop silencing",
                                    idle,
                                    Emphasis::Primary,
                                    cx.theme(),
                                )
                                .when(idle, |b| b.on_click(expire))
                            } else {
                                button_icon(
                                    "alerts-silence",
                                    IconName::BellOff,
                                    "Silence",
                                    idle,
                                    Emphasis::Primary,
                                    cx.theme(),
                                )
                                .when(idle, |b| b.on_click(silence))
                            })
                            .when_some(open, |row, open| {
                                row.child(
                                    button_icon(
                                        "alerts-open",
                                        IconName::FileCode,
                                        "Open YAML",
                                        true,
                                        Emphasis::Plain,
                                        cx.theme(),
                                    )
                                    .on_click(open),
                                )
                            })
                            .when(self.busy, |row| {
                                row.child(div().text_xs().text_color(muted).child("Working…"))
                            }),
                    ),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        match &self.load {
            Load::Loading => message("Asking Alertmanager…", palette.muted).into_any_element(),
            Load::Failed(error) => message(error.clone(), palette.danger).into_any_element(),
            Load::Absent => message(
                "No Alertmanager found in this cluster. kube-prometheus-stack and similar \
                 bundles deploy one.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready if self.lines.is_empty() => {
                message("Nothing is firing.", palette.muted).into_any_element()
            }
            Load::Ready if self.shown.is_empty() => {
                message("Nothing matches.", palette.muted).into_any_element()
            }
            Load::Ready => uniform_list(
                "alerts",
                self.shown.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let theme = cx.theme();
                    let (selected, hover, muted) =
                        (theme.list_active, theme.secondary, theme.muted_foreground);
                    range
                        .filter_map(|position| {
                            let index = *this.shown.get(position)?;
                            let line = this.lines.get(index)?;
                            let color = tone_of(&line.severity).map(|t| tone_color(t, cx.theme()));
                            let is_selected = this.selected.as_ref() == Some(&line.fingerprint);
                            Some(
                                h_flex()
                                    .id(("alert-row", position))
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .px_4()
                                    .gap_3()
                                    .items_center()
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(line.silenced, |row| row.opacity(0.55))
                                    .when(is_selected, |row| row.bg(selected))
                                    .hover(|row| row.bg(hover))
                                    .child(
                                        div()
                                            .size(px(8.))
                                            .flex_none()
                                            .rounded_full()
                                            .bg(color.unwrap_or(muted)),
                                    )
                                    .child(
                                        div()
                                            .w(px(70.))
                                            .flex_none()
                                            .text_xs()
                                            .when_some(color, |c, color| c.text_color(color))
                                            .child(line.severity.clone()),
                                    )
                                    .child(
                                        div()
                                            .w(px(220.))
                                            .flex_none()
                                            .truncate()
                                            .child(line.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(muted)
                                            .child(line.summary.clone()),
                                    )
                                    .child(
                                        div()
                                            .w(px(110.))
                                            .flex_none()
                                            .truncate()
                                            .text_color(muted)
                                            .child(line.namespace.clone().unwrap_or_default()),
                                    )
                                    .child(
                                        div()
                                            .w(px(60.))
                                            .flex_none()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(line.age.clone()),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected =
                                            this.lines.get(index).map(|l| l.fingerprint.clone());
                                        cx.notify();
                                    })),
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element(),
        }
    }
}

impl Render for AlertsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (danger, muted) = {
            let theme = cx.theme();
            (theme.danger, theme.muted_foreground)
        };
        let firing = self.lines.iter().filter(|line| !line.silenced).count();
        let silenced = self.lines.len() - firing;
        let source = self
            .target
            .as_ref()
            .map(|t| format!("{}/{}", t.namespace, t.service));
        let detail = self
            .selected_line()
            .map(|line| self.render_detail(line, cx).into_any_element());
        let list = self.render_list(cx);

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
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
                            .child(page_title(IconName::Bell, "Alerts", cx.theme()))
                            .when(firing > 0, |row| {
                                row.child(pill(format!("{firing} firing"), danger))
                            })
                            .when(silenced > 0, |row| {
                                row.child(pill(format!("{silenced} silenced"), muted))
                            })
                            .when_some(source, |row, source| {
                                row.child(div().text_xs().text_color(muted).child(source))
                            })
                            .child(div().flex_1())
                            .child(div().w(px(220.)).child(Input::new(&self.filter))),
                    )
                    .children(matches!(self.load, Load::Ready).then(|| {
                        let (tone, label, detail) = alerts_status(&self.lines);
                        div().px_4().pb_2().child(cards::status_card(
                            tone,
                            label,
                            detail,
                            None,
                            cx.theme(),
                        ))
                    }))
                    .child(list),
            )
            .children(detail)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{alerts_status, line, matching, order, severity_rank, subject_of, tone_of};
    use k8s_app::present::Tone;
    use k8s_metrics::Alert;

    fn alert(name: &str, severity: &str, state: &str) -> Alert {
        let labels: BTreeMap<String, String> = [
            ("alertname", name),
            ("severity", severity),
            ("namespace", "shop"),
            ("pod", "web-1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        Alert {
            fingerprint: format!("fp-{name}"),
            labels,
            annotations: [("summary".to_string(), "it broke".to_string())].into(),
            starts_at: "1970-01-01T00:01:00Z".into(),
            ends_at: String::new(),
            state: state.into(),
            silenced_by: if state == "suppressed" {
                vec!["s1".into()]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn the_status_is_the_worst_alert_still_firing() {
        let lines = |alerts: &[(&str, &str, &str)]| -> Vec<super::Line> {
            alerts
                .iter()
                .map(|(name, severity, state)| line(alert(name, severity, state), 120))
                .collect()
        };
        let (tone, label, _) = alerts_status(&lines(&[]));
        assert_eq!((tone, label.as_str()), (Tone::Ok, "Nothing firing"));
        let (tone, label, _) = alerts_status(&lines(&[("a", "critical", "suppressed")]));
        assert_eq!((tone, label.as_str()), (Tone::Pending, "All silenced"));
        let (tone, label, detail) = alerts_status(&lines(&[
            ("a", "warning", "active"),
            ("b", "critical", "active"),
            ("c", "warning", "suppressed"),
        ]));
        assert_eq!((tone, label.as_str()), (Tone::Error, "2 firing"));
        assert_eq!(detail, "1 urgent · 1 silenced");
        let (tone, _, detail) = alerts_status(&lines(&[("a", "warning", "active")]));
        assert_eq!(tone, Tone::Warn);
        assert_eq!(detail, "none silenced");
    }

    #[test]
    fn urgent_severities_rank_first_and_only_serious_ones_are_errors() {
        assert!(severity_rank("critical") < severity_rank("warning"));
        assert!(severity_rank("warning") < severity_rank("none"));
        assert_eq!(tone_of("critical"), Some(Tone::Error));
        assert_eq!(tone_of("warning"), Some(Tone::Warn));
        assert_eq!(tone_of("none"), None);
    }

    #[test]
    fn a_line_carries_its_summary_age_and_silence_state() {
        let firing = line(alert("A", "warning", "active"), 120);
        assert_eq!(&*firing.summary, "it broke");
        assert_eq!(&*firing.age, "1m0s");
        assert!(!firing.silenced);
        let quiet = line(alert("B", "warning", "suppressed"), 120);
        assert!(quiet.silenced);
        assert_eq!(quiet.silences, ["s1"]);
    }

    #[test]
    fn firing_comes_before_silenced_whatever_the_severity() {
        let mut lines = vec![
            line(alert("Quiet", "critical", "suppressed"), 0),
            line(alert("Minor", "info", "active"), 0),
            line(alert("Major", "critical", "active"), 0),
        ];
        order(&mut lines);
        let names: Vec<&str> = lines.iter().map(|l| &*l.name).collect();
        assert_eq!(names, ["Major", "Minor", "Quiet"]);
    }

    #[test]
    fn an_alert_about_a_pod_can_open_it() {
        let l = line(alert("A", "warning", "active"), 0);
        let subject =
            subject_of(&l.labels).map(|(resource, kind, name)| (resource, kind, name.to_string()));
        assert_eq!(subject, Some(("core/v1/pods", "Pod", "web-1".to_string())));
    }

    #[test]
    fn the_filter_matches_name_severity_summary_or_namespace() {
        let lines = vec![
            line(alert("DiskFull", "critical", "active"), 0),
            line(alert("Latency", "warning", "active"), 0),
        ];
        assert_eq!(matching(&lines, "disk"), [0]);
        assert_eq!(matching(&lines, "WARNING"), [1]);
        assert_eq!(matching(&lines, "shop"), [0, 1]);
        assert_eq!(matching(&lines, "nope"), Vec::<usize>::new());
    }
}
