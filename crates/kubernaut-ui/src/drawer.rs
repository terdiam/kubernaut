//! The detail drawer for one selected object.
//!
//! A self-contained view: it loads its own data on the tokio runtime and tells
//! its owner only that it wants to close, so the app view stays small.

use std::{cmp::Reverse, ops::Range, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon,
        button::{Button, ButtonVariants},
        h_flex,
        menu::DropdownMenu,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, present};
use k8s_ops::{
    diagnose::StepAction,
    logs::LogTarget,
    related::{self, EventRow},
};
use tokio::runtime::Handle;

use crate::{
    diagnose::{DiagnoseEvent, DiagnoseView, kind_of},
    log_buffer::log_target,
    logs::LogView,
    metrics::{MetricsView, metric_target},
    quota::QuotaView,
    row_actions::{self, RowAction},
    terminal::TerminalView,
    ui::{segment_icon, segmented_bar},
    yaml::YamlView,
};

const ROW_HEIGHT: Pixels = px(28.);

/// What the drawer is about.
#[derive(Clone)]
pub struct Target {
    pub cluster: SharedString,
    /// `group/version/plural`.
    pub resource: SharedString,
    pub kind: SharedString,
    pub namespace: Option<SharedString>,
    pub name: SharedString,
}

impl Target {
    /// This object as a sidebar pin.
    pub fn pin(&self) -> k8s_app::preferences::PinnedObject {
        k8s_app::preferences::PinnedObject {
            cluster: self.cluster.to_string(),
            resource_key: self.resource.to_string(),
            namespace: self.namespace.as_ref().map(ToString::to_string),
            name: self.name.to_string(),
            label: None,
            kind: Some(self.kind.to_string()),
        }
    }
}

pub enum DrawerEvent {
    Close,
    /// An entry of the actions menu, for the app to carry out.
    Row(RowAction),
    /// Another object a diagnosis points at, to open in its place.
    Open(Target),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Yaml,
    Events,
    Logs,
    Terminal,
    /// Why a pod, or a workload's pods, are not running.
    Diagnose,
    /// CPU and memory over time, against what is declared.
    Metrics,
    /// A namespace's resource quota and limit ranges.
    Quota,
}

/// A value that arrives from the cluster.
enum Load<T> {
    Loading,
    Ready(T),
    Failed(SharedString),
}

/// One event as the list shows it. Labels are built once, at load.
struct EventLine {
    warning: bool,
    reason: SharedString,
    message: SharedString,
    count: SharedString,
    age: SharedString,
}

pub struct DrawerView {
    core: Arc<AppCore>,
    runtime: Handle,
    target: Target,
    /// `None` when the object has no logs of its own.
    log_target: Option<LogTarget>,
    /// Created when the tab is first opened, so closed tabs stream nothing.
    logs: Option<Entity<LogView>>,
    /// A shell, only for pods, opened when its tab is first shown.
    terminal: Option<Entity<TerminalView>>,
    /// Created when the tab is first opened, so a closed tab reads nothing.
    diagnose: Option<(Entity<DiagnoseView>, Subscription)>,
    metrics: Option<Entity<MetricsView>>,
    quota: Option<Entity<QuotaView>>,
    tab: Tab,
    yaml: Entity<YamlView>,
    events: Load<Vec<EventLine>>,
    events_scroll: UniformListScrollHandle,
}

impl EventEmitter<DrawerEvent> for DrawerView {}

impl DrawerView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let yaml = cx.new(|cx| YamlView::new(core, runtime, &target, window, cx));

        let events_task = runtime.spawn(fetch_events(Arc::clone(core), target.clone()));
        cx.spawn(async move |this, cx| {
            let result = events_task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.events = match result {
                    Ok(lines) => Load::Ready(lines),
                    Err(message) => Load::Failed(message.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();

        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            log_target: log_target(&target.resource, target.namespace.as_deref(), &target.name),
            logs: None,
            terminal: None,
            diagnose: None,
            metrics: None,
            quota: None,
            target,
            tab: Tab::Yaml,
            yaml,
            events: Load::Loading,
            events_scroll: UniformListScrollHandle::new(),
        }
    }

    /// Switch to a tab, starting its stream if it has one. A tab the object
    /// does not have is ignored.
    pub fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        match tab {
            Tab::Logs if self.log_target.is_none() => return,
            Tab::Terminal if !self.has_shell() => return,
            Tab::Diagnose if !self.can_diagnose() => return,
            Tab::Metrics if metric_target(&self.target).is_none() => return,
            Tab::Quota if !self.has_quota() => return,
            _ => {}
        }
        self.tab = tab;
        match tab {
            Tab::Logs => self.ensure_logs(cx),
            Tab::Terminal => self.ensure_terminal(window, cx),
            Tab::Diagnose => self.ensure_diagnose(window, cx),
            Tab::Metrics => self.ensure_metrics(cx),
            Tab::Quota => self.ensure_quota(cx),
            Tab::Yaml | Tab::Events => {}
        }
        cx.notify();
    }

    /// Whether this drawer is showing `target`.
    pub fn is_for(&self, target: &Target) -> bool {
        self.target.cluster == target.cluster
            && self.target.resource == target.resource
            && self.target.namespace == target.namespace
            && self.target.name == target.name
    }

    fn render_tab(
        &self,
        tab: Tab,
        icon: IconName,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        segment_icon(label, icon, label, self.tab == tab, cx.theme())
            .on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
    }

    fn ensure_logs(&mut self, cx: &mut Context<Self>) {
        if self.logs.is_some() {
            return;
        }
        let Some(log_target) = self.log_target.clone() else {
            return;
        };
        let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
        self.logs = Some(cx.new(|cx| LogView::new(core, runtime, target, log_target, cx)));
    }

    /// A pod, or a workload whose pods can be looked at, can be diagnosed.
    fn can_diagnose(&self) -> bool {
        self.target.namespace.is_some()
            && (self.target.resource == "core/v1/pods"
                || row_actions::is_workload(&self.target.resource))
    }

    fn has_quota(&self) -> bool {
        self.target.resource == "core/v1/namespaces"
    }

    fn ensure_metrics(&mut self, cx: &mut Context<Self>) {
        if self.metrics.is_some() {
            return;
        }
        let Some(metric) = metric_target(&self.target) else {
            return;
        };
        let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
        self.metrics = Some(cx.new(|cx| MetricsView::new(core, runtime, target, metric, cx)));
    }

    fn ensure_quota(&mut self, cx: &mut Context<Self>) {
        if self.quota.is_some() {
            return;
        }
        let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
        self.quota = Some(cx.new(|cx| {
            QuotaView::new(
                core,
                runtime,
                target.cluster.clone(),
                target.name.clone(),
                cx,
            )
        }));
    }

    fn ensure_diagnose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diagnose.is_some() {
            return;
        }
        let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
        let view = cx.new(|cx| DiagnoseView::new(core, runtime, target, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &DiagnoseEvent, window, cx| match event {
                DiagnoseEvent::Action(action) => this.follow(action, window, cx),
            },
        );
        self.diagnose = Some((view, events));
    }

    /// Carry out what a diagnosis step asks: go to a tab, or to another object.
    fn follow(&mut self, action: &StepAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            StepAction::Logs { .. } => self.show(Tab::Logs, window, cx),
            StepAction::Terminal => self.show(Tab::Terminal, window, cx),
            StepAction::Edit => self.show(Tab::Yaml, window, cx),
            StepAction::Open {
                resource,
                namespace,
                name,
            } => cx.emit(DrawerEvent::Open(Target {
                cluster: self.target.cluster.clone(),
                resource: resource.clone().into(),
                kind: kind_of(resource).into(),
                namespace: namespace.clone().map(Into::into),
                name: name.clone().into(),
            })),
        }
    }

    /// Only a pod has a shell to open.
    fn has_shell(&self) -> bool {
        self.target.resource == "core/v1/pods" && self.target.namespace.is_some()
    }

    fn ensure_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal.is_none() {
            let Some(namespace) = self.target.namespace.as_ref().map(ToString::to_string) else {
                return;
            };
            let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
            self.terminal =
                Some(cx.new(|cx| TerminalView::new(core, runtime, target, namespace, cx)));
        }
        if let Some(terminal) = &self.terminal {
            let focus = terminal.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
    }

    fn render_events(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let message = |text: SharedString, color: Hsla| {
            div()
                .p_4()
                .text_sm()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        match &self.events {
            Load::Loading => message("Loading events…".into(), theme.muted_foreground),
            Load::Failed(error) => message(error.clone(), theme.danger),
            Load::Ready(lines) if lines.is_empty() => {
                message("No events.".into(), theme.muted_foreground)
            }
            Load::Ready(lines) => uniform_list(
                "events",
                lines.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let theme = cx.theme();
                    let Load::Ready(lines) = &this.events else {
                        return Vec::new();
                    };
                    lines
                        .get(range.clone())
                        .unwrap_or_default()
                        .iter()
                        .zip(range)
                        .map(|(line, index)| {
                            // `w_full`: without it the row is as wide as its
                            // longest message, and `truncate` has nothing to cut.
                            h_flex()
                                .id(("event", index))
                                .w_full()
                                .h(ROW_HEIGHT)
                                .px_3()
                                .gap_3()
                                .items_center()
                                .text_sm()
                                .child(div().size(px(8.)).flex_none().rounded_full().bg(
                                    if line.warning {
                                        theme.warning
                                    } else {
                                        theme.muted_foreground
                                    },
                                ))
                                .child(
                                    div()
                                        .w(px(120.))
                                        .flex_none()
                                        .truncate()
                                        .child(line.reason.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(line.message.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(line.count.clone()),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(line.age.clone()),
                                )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.events_scroll)
            .flex_1()
            .into_any_element(),
        }
    }
}

impl Render for DrawerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, border, muted, secondary, hover) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.secondary,
            theme.secondary_hover,
        );

        let kind = div()
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .bg(secondary)
            .text_xs()
            .child(self.target.kind.clone());
        let subtitle = h_flex()
            .gap_2()
            .items_center()
            .child(kind)
            .children(self.target.namespace.clone());

        v_flex()
            .w(px(560.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(52.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_semibold()
                                    .truncate()
                                    .child(self.target.name.clone()),
                            )
                            .child(div().text_xs().text_color(muted).child(subtitle)),
                    )
                    .child({
                        let owner = cx.entity().downgrade();
                        let resource = self.target.resource.clone();
                        let pinned = self.core.preferences().is_pinned(&self.target.pin());
                        Button::new("drawer-actions")
                            .ghost()
                            .compact()
                            .icon(IconName::Ellipsis)
                            .tooltip("Actions")
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                let owner = owner.clone();
                                row_actions::build_menu(
                                    menu,
                                    &resource,
                                    false,
                                    pinned,
                                    move |action, _, cx| {
                                        owner
                                            .update(cx, |_, cx| cx.emit(DrawerEvent::Row(action)))
                                            .ok();
                                    },
                                )
                            })
                    })
                    .child(
                        div()
                            .id("close-drawer")
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(|button| button.bg(hover))
                            .tooltip(|window, cx| Tooltip::new("Close").build(window, cx))
                            .child(Icon::new(IconName::Close).size_4())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DrawerEvent::Close))),
                    ),
            )
            .child(
                div().flex_none().px_3().py_2().child(
                    segmented_bar(cx.theme())
                        .child(self.render_tab(Tab::Yaml, IconName::FileCode, "YAML", cx))
                        .child(self.render_tab(Tab::Events, IconName::Activity, "Events", cx))
                        .when(metric_target(&self.target).is_some(), |tabs| {
                            tabs.child(self.render_tab(
                                Tab::Metrics,
                                IconName::Gauge,
                                "Metrics",
                                cx,
                            ))
                        })
                        .when(self.has_quota(), |tabs| {
                            tabs.child(self.render_tab(Tab::Quota, IconName::Ruler, "Quota", cx))
                        })
                        .when(self.can_diagnose(), |tabs| {
                            tabs.child(self.render_tab(
                                Tab::Diagnose,
                                IconName::ShieldAlert,
                                "Diagnose",
                                cx,
                            ))
                        })
                        .when(self.log_target.is_some(), |tabs| {
                            tabs.child(self.render_tab(Tab::Logs, IconName::ScrollText, "Logs", cx))
                        })
                        .when(self.has_shell(), |tabs| {
                            tabs.child(self.render_tab(
                                Tab::Terminal,
                                IconName::SquareTerminal,
                                "Terminal",
                                cx,
                            ))
                        }),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(match self.tab {
                        Tab::Yaml => self.yaml.clone().into_any_element(),
                        Tab::Events => self.render_events(cx),
                        Tab::Logs => match &self.logs {
                            Some(logs) => logs.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                        Tab::Metrics => match &self.metrics {
                            Some(metrics) => metrics.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                        Tab::Quota => match &self.quota {
                            Some(quota) => quota.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                        Tab::Diagnose => match &self.diagnose {
                            Some((diagnose, _)) => diagnose.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                        Tab::Terminal => match &self.terminal {
                            Some(terminal) => terminal.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                    }),
            )
    }
}

async fn fetch_events(core: Arc<AppCore>, target: Target) -> Result<Vec<EventLine>, String> {
    let handle = core
        .clusters
        .require(&target.cluster)
        .map_err(|err| err.to_string())?;
    let rows = related::events(&handle, target.namespace.as_deref(), &target.name)
        .await
        .map_err(|err| err.to_string())?;
    Ok(event_lines(&rows, present::now()))
}

/// Newest first, with the labels the list draws.
fn event_lines(rows: &[EventRow], now: i64) -> Vec<EventLine> {
    let mut keyed: Vec<(i64, &EventRow)> = rows
        .iter()
        .map(|row| {
            let seen = row
                .last_seen
                .as_deref()
                .or(row.first_seen.as_deref())
                .and_then(present::parse_timestamp);
            (seen.unwrap_or(i64::MIN), row)
        })
        .collect();
    keyed.sort_by_key(|&(seen, _)| Reverse(seen));

    let mut age = String::new();
    keyed
        .into_iter()
        .map(|(seen, row)| {
            age.clear();
            if seen != i64::MIN {
                present::write_age(&mut age, now - seen);
            }
            EventLine {
                warning: row.kind == "Warning",
                reason: row.reason.as_str().into(),
                message: row.message.as_str().into(),
                count: if row.count > 1 {
                    format!("×{}", row.count).into()
                } else {
                    SharedString::default()
                },
                age: age.as_str().into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::event_lines;
    use k8s_ops::related::EventRow;

    fn event(kind: &str, reason: &str, count: i32, last_seen: Option<&str>) -> EventRow {
        EventRow {
            kind: kind.into(),
            reason: reason.into(),
            message: format!("{reason} happened"),
            count,
            first_seen: None,
            last_seen: last_seen.map(Into::into),
            source: None,
            object: String::new(),
        }
    }

    #[test]
    fn events_are_newest_first_with_ages_and_repeat_counts() {
        let rows = [
            event("Normal", "Pulled", 1, Some("1970-01-01T00:01:00Z")),
            event("Warning", "BackOff", 7, Some("1970-01-01T00:10:00Z")),
            event("Normal", "Created", 1, None),
        ];
        let lines = event_lines(&rows, 660);

        let reasons: Vec<_> = lines.iter().map(|l| &*l.reason).collect();
        assert_eq!(
            reasons,
            ["BackOff", "Pulled", "Created"],
            "undated events last"
        );

        assert!(lines[0].warning);
        assert_eq!(&*lines[0].count, "×7");
        assert_eq!(&*lines[0].age, "1m0s");
        assert_eq!(&*lines[1].count, "", "a single occurrence shows no count");
        assert_eq!(&*lines[2].age, "", "no timestamp, no age");
    }
}
