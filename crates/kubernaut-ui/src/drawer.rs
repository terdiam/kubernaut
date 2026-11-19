//! The detail drawer for one selected object.
//!
//! A self-contained view: it loads its own data on the tokio runtime and tells
//! its owner only that it wants to close, so the app view stays small.

use std::{cmp::Reverse, ops::Range, sync::Arc};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, Icon, WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        h_flex,
        menu::DropdownMenu,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, object_ops, present};
use k8s_ops::{
    diagnose::StepAction,
    logs::LogTarget,
    related::{self, EventRow},
};
use tokio::runtime::Handle;

use crate::{
    cards,
    details::{DetailsEvent, DetailsView},
    diagnose::{DiagnoseEvent, DiagnoseView, kind_of},
    log_buffer::log_target,
    logs::LogView,
    metrics::{MetricsView, metric_target},
    node_edit::NodeEditView,
    quota::QuotaView,
    row_actions::{self, RowAction},
    terminal::{Request, TerminalView},
    ui::{
        Emphasis, button_icon, chip, grid_cell, grid_header, segment_icon, segmented_bar,
        toast_success,
    },
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
    /// What the object is made of and how it is doing, read for a person.
    Details,
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
    /// A debug container added to the pod.
    Debug,
    /// A root shell on the node.
    NodeShell,
    /// A node's labels and taints.
    Labels,
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
    /// A debug container's terminal, once the person has agreed to add one.
    debug: Option<Entity<TerminalView>>,
    /// A node shell's terminal, once the person has agreed to open one.
    node_shell: Option<Entity<TerminalView>>,
    /// Created when the tab is first opened, so a closed tab reads nothing.
    diagnose: Option<(Entity<DiagnoseView>, Subscription)>,
    metrics: Option<Entity<MetricsView>>,
    quota: Option<Entity<QuotaView>>,
    /// Created when the tab is first opened.
    node_edit: Option<Entity<NodeEditView>>,
    tab: Tab,
    details: Entity<DetailsView>,
    _details_events: Subscription,
    yaml: Entity<YamlView>,
    events: Load<Vec<EventLine>>,
    events_scroll: UniformListScrollHandle,
    /// Show only the events that warn, so a noisy object's problem is found.
    warnings_only: bool,
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
        let details = cx.new(|cx| DetailsView::new(core, runtime, &target, cx));
        let details_events = cx.subscribe(&details, |_, _, event: &DetailsEvent, cx| match event {
            DetailsEvent::Open(target) => cx.emit(DrawerEvent::Open(target.clone())),
        });

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
            debug: None,
            node_shell: None,
            diagnose: None,
            metrics: None,
            quota: None,
            node_edit: None,
            target,
            tab: Tab::Details,
            details,
            _details_events: details_events,
            yaml,
            events: Load::Loading,
            events_scroll: UniformListScrollHandle::new(),
            warnings_only: false,
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
            Tab::Debug if !self.has_shell() => return,
            Tab::NodeShell | Tab::Labels if !self.has_node_shell() => return,
            _ => {}
        }
        self.tab = tab;
        match tab {
            Tab::Logs => self.ensure_logs(window, cx),
            Tab::Terminal => self.ensure_terminal(window, cx),
            Tab::Diagnose => self.ensure_diagnose(window, cx),
            Tab::Metrics => self.ensure_metrics(cx),
            Tab::Quota => self.ensure_quota(cx),
            Tab::Labels => self.ensure_node_edit(window, cx),
            // These change the cluster, so they wait for the person to say yes.
            Tab::Details | Tab::Yaml | Tab::Events | Tab::Debug | Tab::NodeShell => {}
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

    fn ensure_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.logs.is_some() {
            return;
        }
        let Some(log_target) = self.log_target.clone() else {
            return;
        };
        let (core, runtime, target) = (&self.core, &self.runtime, &self.target);
        self.logs = Some(cx.new(|cx| LogView::new(core, runtime, target, log_target, window, cx)));
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
            let request = Request::Shell {
                namespace,
                pod: self.target.name.to_string(),
            };
            self.terminal = Some(self.open_terminal(request, cx));
        }
        if let Some(terminal) = &self.terminal {
            let focus = terminal.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
    }

    fn open_terminal(&self, request: Request, cx: &mut Context<Self>) -> Entity<TerminalView> {
        let (core, runtime, cluster) = (&self.core, &self.runtime, self.target.cluster.clone());
        cx.new(|cx| TerminalView::new(core, runtime, cluster, request, cx))
    }

    fn ensure_node_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.node_edit.is_some() {
            return;
        }
        let (core, runtime) = (&self.core, &self.runtime);
        let (cluster, node) = (self.target.cluster.clone(), self.target.name.clone());
        self.node_edit =
            Some(cx.new(|cx| NodeEditView::new(core, runtime, cluster, node, window, cx)));
    }

    fn has_node_shell(&self) -> bool {
        self.target.resource == "core/v1/nodes"
    }

    /// Ask before a terminal that changes the cluster, saying what it leaves.
    fn confirm_session(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        let (title, detail, ok): (SharedString, &'static str, &'static str) = match tab {
            Tab::Debug => (
                format!("Add a debug container to {}?", self.target.name).into(),
                "A temporary busybox container joins this pod and shares its processes. An \
                 ephemeral container cannot be removed: the pod keeps it until it is recreated.",
                "Add it",
            ),
            _ => (
                format!("Open a root shell on {}?", self.target.name).into(),
                "A privileged pod starts on this node with the host's process, network and IPC \
                 namespaces, and you enter it as root. It is deleted when you close this tab.",
                "Open it",
            ),
        };
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog
                .title(title.clone())
                .description(detail)
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| this.start_session(tab, window, cx))
                        .ok();
                    true
                })
        });
    }

    fn start_session(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        let view = match tab {
            Tab::Debug => {
                let Some(namespace) = self.target.namespace.as_ref().map(ToString::to_string)
                else {
                    return;
                };
                let request = Request::Debug {
                    namespace,
                    pod: self.target.name.to_string(),
                };
                let view = self.open_terminal(request, cx);
                self.debug = Some(view.clone());
                view
            }
            _ => {
                let request = Request::Node {
                    node: self.target.name.to_string(),
                };
                let view = self.open_terminal(request, cx);
                self.node_shell = Some(view.clone());
                view
            }
        };
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// What a tab shows before its session has started: what it will do, and the
    /// button that starts it.
    fn render_start(&self, tab: Tab, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (what, label, icon) = match tab {
            Tab::Debug => (
                "Add a temporary debug container to this pod, with a shell and the tools of a \
                 small image, sharing the pod's processes. Use it when the app's own image has \
                 no shell. The container stays until the pod is recreated.",
                "Add a debug container…",
                IconName::SquareTerminal,
            ),
            _ => (
                "Open a root shell on the machine this node runs on, through a privileged pod \
                 that is deleted when you close the tab. This is full access to the host.",
                "Open a node shell…",
                IconName::Server,
            ),
        };
        v_flex()
            .p_4()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(what))
            .child(
                h_flex().child(
                    button_icon(
                        "drawer-start-session",
                        icon,
                        label,
                        true,
                        Emphasis::Danger,
                        cx.theme(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_session(tab, window, cx);
                    })),
                ),
            )
            .into_any_element()
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
        let lines = match &self.events {
            Load::Loading => return message("Loading events…".into(), theme.muted_foreground),
            Load::Failed(error) => return message(error.clone(), theme.danger),
            Load::Ready(lines) if lines.is_empty() => {
                return message("No events.".into(), theme.muted_foreground);
            }
            Load::Ready(lines) => lines,
        };

        let warnings = lines.iter().filter(|line| line.warning).count();
        let only = self.warnings_only && warnings > 0;
        let shown = if only { warnings } else { lines.len() };
        // Newest first, so the first warning is the latest one.
        let latest = lines
            .iter()
            .find(|line| line.warning)
            .map(|l| l.age.clone());
        let status = if warnings == 0 {
            cards::status_card(
                present::Tone::Ok,
                "No warnings",
                format!("{} events", lines.len()),
                None,
                theme,
            )
        } else {
            cards::status_card(
                present::Tone::Warn,
                format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" }),
                format!(
                    "of {} events · the latest {} ago",
                    lines.len(),
                    latest.unwrap_or_default()
                ),
                None,
                theme,
            )
        };

        let filter = |id: &'static str, label: String, active: bool, cx: &mut Context<Self>| {
            let target = id == "warnings";
            chip(id, label, true, active, cx.theme()).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.warnings_only = target;
                    cx.notify();
                },
            ))
        };

        let list = uniform_list(
            "events",
            shown,
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                let theme = cx.theme();
                let Load::Ready(lines) = &this.events else {
                    return Vec::new();
                };
                let only = this.warnings_only && lines.iter().any(|line| line.warning);
                lines
                    .iter()
                    .filter(|line| !only || line.warning)
                    .skip(range.start)
                    .take(range.len())
                    .zip(range)
                    .map(|(line, index)| {
                        let (reason, copy) = (line.reason.clone(), line.message.clone());
                        let (dot, tint) = if line.warning {
                            (theme.warning, theme.warning.opacity(0.07))
                        } else {
                            (theme.muted_foreground, theme.background.opacity(0.0))
                        };
                        let full = line.message.clone();
                        // `w_full`: without it the row is as wide as its
                        // longest message, and `truncate` has nothing to cut.
                        h_flex()
                            .id(("event", index))
                            .w_full()
                            .h(ROW_HEIGHT)
                            .text_sm()
                            .border_b_1()
                            .border_color(theme.border)
                            .bg(tint)
                            .cursor_pointer()
                            .hover(|row| row.bg(theme.secondary))
                            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                            .child(
                                grid_cell(cx.theme(), Some(150.), false)
                                    .child(div().size(px(8.)).flex_none().rounded_full().bg(dot))
                                    .child(
                                        div()
                                            .truncate()
                                            .font_semibold()
                                            .when(line.warning, |name| {
                                                name.text_color(theme.warning)
                                            })
                                            .child(line.reason.clone()),
                                    ),
                            )
                            .child(
                                grid_cell(cx.theme(), None, false)
                                    .child(div().truncate().child(line.message.clone())),
                            )
                            .child(
                                grid_cell(cx.theme(), Some(56.), false).child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(line.count.clone()),
                                ),
                            )
                            .child(
                                grid_cell(cx.theme(), Some(64.), true).child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(line.age.clone()),
                                ),
                            )
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(format!(
                                    "{reason}: {copy}"
                                )));
                                toast_success(window, cx, "Copied the event");
                            })
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.events_scroll)
        .flex_1();

        v_flex()
            .size_full()
            .child(
                v_flex().flex_none().gap_2().p_3().child(status).child(
                    h_flex()
                        .gap_1p5()
                        .child(filter(
                            "all-events",
                            format!("All {}", lines.len()),
                            !only,
                            cx,
                        ))
                        .child(filter("warnings", format!("Warnings {warnings}"), only, cx)),
                ),
            )
            .child(grid_header(
                cx.theme(),
                &[
                    ("Reason", Some(150.)),
                    ("Message", None),
                    ("Count", Some(56.)),
                    ("Age", Some(64.)),
                ],
            ))
            .child(list)
            .into_any_element()
    }
}

impl Render for DrawerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, secondary, hover) = (
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

        // Width, background and the left edge belong to the column that holds
        // this and the tab strip, so one tab and many look alike.
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
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
                        let pinned = self.core.is_pinned(&self.target.pin());
                        let details = self.details.clone();
                        Button::new("drawer-actions")
                            .ghost()
                            .compact()
                            .icon(IconName::Ellipsis)
                            .tooltip("Actions")
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, cx| {
                                let owner = owner.clone();
                                row_actions::build_menu(
                                    menu,
                                    &resource,
                                    false,
                                    pinned,
                                    details.read(cx).suspended(),
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
                    // Eight tabs do not fit in one row of the drawer, and the last
                    // ones (Debug) were cut off; let them wrap onto a second row.
                    segmented_bar(cx.theme())
                        .flex_wrap()
                        .child(self.render_tab(Tab::Details, IconName::Info, "Details", cx))
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
                            .child(self.render_tab(
                                Tab::Debug,
                                IconName::Cog,
                                "Debug",
                                cx,
                            ))
                        })
                        .when(self.has_node_shell(), |tabs| {
                            tabs.child(self.render_tab(
                                Tab::Labels,
                                IconName::Settings2,
                                "Labels & taints",
                                cx,
                            ))
                            .child(self.render_tab(
                                Tab::NodeShell,
                                IconName::SquareTerminal,
                                "Node shell",
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
                        Tab::Details => self.details.clone().into_any_element(),
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
                        Tab::Debug => match &self.debug {
                            Some(terminal) => terminal.clone().into_any_element(),
                            None => self.render_start(Tab::Debug, cx),
                        },
                        Tab::Labels => match &self.node_edit {
                            Some(edit) => edit.clone().into_any_element(),
                            None => div().into_any_element(),
                        },
                        Tab::NodeShell => match &self.node_shell {
                            Some(terminal) => terminal.clone().into_any_element(),
                            None => self.render_start(Tab::NodeShell, cx),
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
    // A workload's own events are mostly "scaled up"; what its replicas say
    // (probe failures, image pulls, evictions) is what the tab is opened for.
    let rows = match target.namespace.as_deref() {
        Some(namespace) if row_actions::is_workload(&target.resource) => {
            object_ops::read_connected(
                &core,
                &target.cluster,
                &target.resource,
                Some(namespace),
                &target.name,
            )
            .await?
            .events
        }
        namespace => related::events(&handle, namespace, &target.name)
            .await
            .map_err(|err| err.to_string())?,
    };
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
