//! GitOps: what Argo CD, Flux and Fleet are syncing, and whether it worked.
//!
//! The list puts the broken things first, because the reason to open it is
//! that something is not applying. Selecting one explains why, and offers the
//! two things people do about it: reconcile now, and pause or resume.

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
use k8s_app::{AppCore, object_ops, present, present::Tone};
use k8s_ops::gitops::GitOpsEntry;
use tokio::runtime::Handle;

use crate::{
    cards::{self, Folds},
    drawer::Target,
    namespaces,
    rows::contains_ignore_case,
    ui::{
        Emphasis, Palette, button_icon, grid_cell, grid_header, message, page_title, pill,
        toast_error, toast_success, tone_color,
    },
};

const ROW_HEIGHT: Pixels = px(32.);
/// How often the list is read again. A reconcile takes seconds, so this keeps
/// the screen close to the truth without a call per second.
const POLL: Duration = Duration::from_secs(15);

pub enum GitOpsEvent {
    /// Show the object's YAML and events.
    Open(Target),
}

/// A revision as a person reads it: a commit hash is its first seven
/// characters, and Flux's `branch@sha1:…` keeps the branch and a short hash.
pub fn short_revision(revision: &str) -> String {
    if let Some((branch, hash)) = revision
        .split_once("@sha1:")
        .or_else(|| revision.split_once("@sha256:"))
    {
        return format!("{branch}@{}", hash.chars().take(7).collect::<String>());
    }
    let is_hash = revision.len() >= 40 && revision.chars().all(|c| c.is_ascii_hexdigit());
    if is_hash {
        revision.chars().take(7).collect()
    } else {
        revision.to_string()
    }
}

fn tone_of(health: &str) -> Option<Tone> {
    match health {
        "ok" => Some(Tone::Ok),
        "pending" => Some(Tone::Pending),
        "warning" => Some(Tone::Warn),
        "error" => Some(Tone::Error),
        _ => None,
    }
}

/// One entry as the list draws it. Labels are built once, at load.
struct Line {
    entry: GitOpsEntry,
    tone: Option<Tone>,
    controller: SharedString,
    source: SharedString,
    /// What is applied, and what it is tracking when that differs.
    revision: SharedString,
    drifted: bool,
    age: SharedString,
}

fn controller_label(controller: &str) -> &'static str {
    match controller {
        "argocd" => "Argo CD",
        "flux" => "Flux",
        "fleet" => "Fleet",
        _ => "GitOps",
    }
}

fn line(entry: GitOpsEntry, now: i64) -> Line {
    let applied = entry.applied_revision.as_deref().map(short_revision);
    let target = entry.target_revision.as_deref().map(short_revision);
    // Tracking `main` and applying a commit is normal; the commit differing
    // from a *pinned* target is not. Only a differing pair is flagged.
    let drifted = matches!((&applied, &target), (Some(a), Some(t)) if a != t && !t.is_empty() && t.len() >= 7 && t.chars().all(|c| c.is_ascii_hexdigit()));
    let revision = match (applied, target) {
        (Some(applied), Some(target)) if applied != target => format!("{applied}  ← {target}"),
        (Some(applied), _) => applied,
        (None, Some(target)) => target,
        (None, None) => String::new(),
    };
    let mut age = String::new();
    if let Some(at) = entry
        .last_sync
        .as_deref()
        .and_then(present::parse_timestamp)
    {
        present::write_age(&mut age, now - at);
    }
    Line {
        tone: tone_of(&entry.health),
        controller: controller_label(&entry.controller).into(),
        source: [entry.source.as_deref(), entry.path.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
            .into(),
        revision: revision.into(),
        drifted,
        age: age.into(),
        entry,
    }
}

/// Indices of the lines a filter keeps.
fn matching(lines: &[Line], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            [
                line.entry.name.as_str(),
                line.entry.namespace.as_deref().unwrap_or_default(),
                line.entry.status.as_str(),
                &line.controller,
                &line.source,
            ]
            .into_iter()
            .any(|field| contains_ignore_case(field, needle))
        })
        .map(|(index, _)| index)
        .collect()
}

/// How the synced objects stand, for the card above the list: failing ones
/// first, then those still working, then drift and suspension, else all synced.
fn gitops_status(lines: &[Line]) -> (Tone, String, String) {
    let count = |wanted: fn(&Line) -> bool| lines.iter().filter(|line| wanted(line)).count();
    let failing = count(|line| line.tone == Some(Tone::Error));
    let working = count(|line| matches!(line.tone, Some(Tone::Pending | Tone::Warn)));
    let drifted = count(|line| line.drifted);
    let suspended = count(|line| line.entry.suspended);
    let total = lines.len();
    let mut notes = vec![format!("of {total}")];
    if drifted > 0 {
        notes.push(format!("{drifted} drifted"));
    }
    if suspended > 0 {
        notes.push(format!("{suspended} suspended"));
    }
    let detail = notes.join(" · ");
    if failing > 0 {
        (Tone::Error, format!("{failing} failing"), detail)
    } else if working > 0 {
        (Tone::Warn, format!("{working} not settled"), detail)
    } else if drifted > 0 {
        (
            Tone::Warn,
            format!("{drifted} drifted"),
            format!("of {total}"),
        )
    } else {
        (Tone::Ok, "All synced".to_string(), detail)
    }
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

/// What the buttons do, for the confirmation and the toast.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Reconcile,
    Suspend,
    Resume,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Self::Reconcile => "Reconcile",
            Self::Suspend => "Suspend",
            Self::Resume => "Resume",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Reconcile => "The controller is asked to sync it now.",
            Self::Suspend => "The controller stops applying changes until it is resumed.",
            Self::Resume => "The controller starts applying changes again.",
        }
    }
}

pub struct GitOpsView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    scope: Vec<SharedString>,
    load: Load,
    lines: Vec<Line>,
    controllers: Vec<SharedString>,
    limitations: Vec<SharedString>,
    shown: Vec<usize>,
    filter: Entity<InputState>,
    /// Identifies the row across refreshes, which re-sort the list.
    selected: Option<(SharedString, Option<SharedString>, SharedString)>,
    busy: Option<&'static str>,
    scroll: UniformListScrollHandle,
    _filter: Subscription,
    folds: Folds,
}

impl EventEmitter<GitOpsEvent> for GitOpsView {}

impl GitOpsView {
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
            lines: Vec::new(),
            controllers: Vec::new(),
            limitations: Vec::new(),
            shown: Vec::new(),
            filter,
            selected: None,
            busy: None,
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
        // One namespace is asked of the server; several are read cluster-wide
        // and narrowed here, as the survey takes only one.
        let (namespace, scope) = namespaces::watch_scope(&self.scope);
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            let mut summary =
                object_ops::gitops_survey(&core, &cluster, namespace.as_deref()).await?;
            summary
                .entries
                .retain(|entry| namespaces::admits(&scope, entry.namespace.as_deref()));
            Ok::<_, String>(summary)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_refresh(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_refresh(
        &mut self,
        result: Result<k8s_ops::gitops::GitOpsSummary, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(summary) => {
                let now = present::now();
                self.lines = summary.entries.into_iter().map(|e| line(e, now)).collect();
                self.controllers = summary
                    .controllers
                    .iter()
                    .map(|c| controller_label(c).into())
                    .collect();
                self.limitations = summary.limitations.into_iter().map(Into::into).collect();
                self.load = Load::Ready;
                self.apply_filter(cx);
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

    fn key_of(line: &Line) -> (SharedString, Option<SharedString>, SharedString) {
        (
            line.entry.resource.as_str().into(),
            line.entry.namespace.as_deref().map(Into::into),
            line.entry.name.as_str().into(),
        )
    }

    fn selected_line(&self) -> Option<&Line> {
        let key = self.selected.as_ref()?;
        self.lines.iter().find(|line| &Self::key_of(line) == key)
    }

    fn target(line: &Line, cluster: &SharedString) -> Target {
        Target {
            cluster: cluster.clone(),
            resource: line.entry.resource.as_str().into(),
            kind: line.entry.kind.as_str().into(),
            namespace: line.entry.namespace.as_deref().map(Into::into),
            name: line.entry.name.as_str().into(),
        }
    }

    fn confirm(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.selected_line() else {
            return;
        };
        let question: SharedString =
            format!("{} {} {}?", action.verb(), line.entry.kind, line.entry.name).into();
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog
                .title(question.clone())
                .description(action.detail())
                .ok_text(action.verb())
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
        let Some(line) = self.selected_line() else {
            return;
        };
        let (resource, namespace, name) = (
            line.entry.resource.clone(),
            line.entry.namespace.clone(),
            line.entry.name.clone(),
        );
        self.busy = Some("Working…");
        cx.notify();

        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let label = name.clone();
        let task = self.runtime.spawn(async move {
            match action {
                Action::Reconcile => {
                    object_ops::gitops_reconcile(
                        &core,
                        &cluster,
                        &resource,
                        namespace.as_deref(),
                        &name,
                    )
                    .await
                }
                Action::Suspend | Action::Resume => {
                    object_ops::gitops_set_suspended(
                        &core,
                        &cluster,
                        &resource,
                        namespace.as_deref(),
                        &name,
                        action == Action::Suspend,
                    )
                    .await
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        let done = match action {
                            Action::Reconcile => format!("Asked to reconcile {label}"),
                            Action::Suspend => format!("Suspended {label}"),
                            Action::Resume => format!("Resumed {label}"),
                        };
                        toast_success(window, cx, done);
                        this.refresh(cx);
                    }
                    Err(reason) => toast_error(
                        window,
                        cx,
                        format!("Could not {} {label}", action.verb().to_lowercase()),
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
        let (background, border, muted, danger) = (
            theme.background,
            theme.border,
            theme.muted_foreground,
            theme.danger,
        );
        let entry = &line.entry;
        let none = || "—".to_string();
        let rows: Vec<(&str, String)> = vec![
            ("Namespace", entry.namespace.clone().unwrap_or_else(none)),
            ("Source", entry.source.clone().unwrap_or_else(none)),
            ("Path", entry.path.clone().unwrap_or_else(none)),
            (
                "Tracking",
                entry.target_revision.clone().unwrap_or_else(none),
            ),
            (
                "Applied",
                entry.applied_revision.clone().unwrap_or_else(none),
            ),
            (
                "Last sync",
                if line.age.is_empty() {
                    none()
                } else {
                    format!("{} ago", line.age)
                },
            ),
        ];
        let id = "gitops-source";
        let folded = self.folds.is_folded(id);
        let fold = cx.listener(move |this, _, _, cx| {
            this.folds.toggle(id);
            cx.notify();
        });
        let cards_column = v_flex()
            .gap_2()
            .child(cards::card(
                id,
                "Source and revision",
                Vec::new(),
                folded,
                fold,
                rows.iter()
                    .map(|(label, value)| cards::field("gitops", label, value, 110., cx.theme()))
                    .collect(),
                cx.theme(),
            ))
            .when(line.drifted, |column| {
                column.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child("The applied commit is not the one this is pinned to."),
                )
            });
        let idle = self.busy.is_none();
        let target = Self::target(line, &self.cluster);

        let reconcile =
            cx.listener(|this, _, window, cx| this.confirm(Action::Reconcile, window, cx));
        let toggle = {
            let action = if entry.suspended {
                Action::Resume
            } else {
                Action::Suspend
            };
            cx.listener(move |this, _, window, cx| this.confirm(action, window, cx))
        };
        let open = cx.listener(move |_, _, _, cx| cx.emit(GitOpsEvent::Open(target.clone())));

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
                    .child(div().font_semibold().child(entry.name.clone()))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{} · {}", line.controller, entry.kind)),
                            )
                            .child(pill(
                                entry.status.clone(),
                                line.tone.map_or(muted, |tone| tone_color(tone, cx.theme())),
                            ))
                            .when(entry.suspended && entry.status != "Suspended", |row| {
                                row.child(pill("Suspended", cx.theme().warning))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("gitops-detail")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .gap_2()
                    .when_some(entry.message.clone(), |column, text| {
                        column.child(
                            div()
                                .p_2()
                                .mb_1()
                                .rounded_md()
                                .text_sm()
                                .bg(danger.opacity(0.1))
                                .text_color(if line.tone == Some(Tone::Error) {
                                    danger
                                } else {
                                    muted
                                })
                                .child(text),
                        )
                    })
                    .child(cards_column),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        button_icon(
                            "gitops-reconcile",
                            IconName::RefreshCw,
                            "Reconcile now",
                            idle && entry.reconcilable,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(idle && entry.reconcilable, |b| b.on_click(reconcile)),
                    )
                    .when(entry.controller == "flux", |row| {
                        row.child(
                            button_icon(
                                "gitops-suspend",
                                if entry.suspended {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                },
                                if entry.suspended { "Resume" } else { "Suspend" },
                                idle,
                                Emphasis::Plain,
                                cx.theme(),
                            )
                            .when(idle, |b| b.on_click(toggle)),
                        )
                    })
                    .child(
                        button_icon(
                            "gitops-open",
                            IconName::FileCode,
                            "Open YAML",
                            true,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .on_click(open),
                    )
                    .when_some(self.busy, |row, text| {
                        row.child(div().text_xs().text_color(muted).child(text))
                    }),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = Palette::of(cx);
        match &self.load {
            Load::Loading => message("Reading the cluster…", palette.muted).into_any_element(),
            Load::Failed(error) => message(error.clone(), palette.danger).into_any_element(),
            Load::Ready if self.controllers.is_empty() => message(
                "No GitOps controller found. Argo CD, Flux and Fleet appear here when installed.",
                palette.muted,
            )
            .into_any_element(),
            Load::Ready if self.shown.is_empty() => {
                message("Nothing matches.", palette.muted).into_any_element()
            }
            Load::Ready => uniform_list(
                "gitops",
                self.shown.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let theme = cx.theme();
                    let (selected, hover, muted, warning, line_color) = (
                        theme.list_active,
                        theme.secondary,
                        theme.muted_foreground,
                        theme.warning,
                        theme.border,
                    );
                    range
                        .filter_map(|position| {
                            let index = *this.shown.get(position)?;
                            let line = this.lines.get(index)?;
                            let color = line.tone.map(|tone| tone_color(tone, cx.theme()));
                            let is_selected = this.selected.as_ref() == Some(&Self::key_of(line));
                            Some(
                                h_flex()
                                    .id(("gitops-row", position))
                                    .w_full()
                                    .h(ROW_HEIGHT)
                                    .text_sm()
                                    .cursor_pointer()
                                    .border_b_1()
                                    .border_color(line_color)
                                    .when(is_selected, |row| row.bg(selected))
                                    .hover(|row| row.bg(hover))
                                    .child(
                                        grid_cell(cx.theme(), None, false)
                                            .child(
                                                div()
                                                    .size(px(8.))
                                                    .flex_none()
                                                    .rounded_full()
                                                    .bg(color.unwrap_or(muted)),
                                            )
                                            .child(div().truncate().child(line.entry.name.clone())),
                                    )
                                    .child(grid_cell(cx.theme(), Some(90.), false).child(
                                        div().text_color(muted).child(line.controller.clone()),
                                    ))
                                    .child(grid_cell(cx.theme(), Some(130.), false).child(
                                        div().truncate().text_color(muted).child(
                                            line.entry.namespace.clone().unwrap_or_default(),
                                        ),
                                    ))
                                    .child(
                                        grid_cell(cx.theme(), Some(150.), false).child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .when(line.drifted, |c| c.text_color(warning))
                                                .child(line.revision.clone()),
                                        ),
                                    )
                                    .child(
                                        grid_cell(cx.theme(), Some(110.), true).child(
                                            div()
                                                .truncate()
                                                .when_some(color, |c, color| c.text_color(color))
                                                .child(line.entry.status.clone()),
                                        ),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected = this.lines.get(index).map(Self::key_of);
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

impl Render for GitOpsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let failing = self
            .lines
            .iter()
            .filter(|line| matches!(line.tone, Some(Tone::Error | Tone::Warn)))
            .count();
        let danger = cx.theme().danger;
        let detail = self
            .selected_line()
            .map(|line| self.render_detail(line, cx).into_any_element());

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
                            .child(page_title(IconName::GitBranch, "GitOps", cx.theme()))
                            .children(
                                self.controllers
                                    .iter()
                                    .map(|c| pill(c.clone(), palette.muted)),
                            )
                            .when(failing > 0, |row| {
                                row.child(pill(format!("{failing} need attention"), danger))
                            })
                            .child(div().flex_1())
                            .child(div().w(px(220.)).child(Input::new(&self.filter))),
                    )
                    .children(
                        (matches!(self.load, Load::Ready) && !self.lines.is_empty()).then(|| {
                            let (tone, label, detail) = gitops_status(&self.lines);
                            div().px_4().pb_2().child(cards::status_card(
                                tone,
                                label,
                                detail,
                                None,
                                cx.theme(),
                            ))
                        }),
                    )
                    .children(self.limitations.iter().take(3).map(|text| {
                        div()
                            .px_4()
                            .pb_1()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(text.clone())
                    }))
                    .when(
                        matches!(self.load, Load::Ready) && !self.shown.is_empty(),
                        |column| {
                            column.child(grid_header(
                                cx.theme(),
                                &[
                                    ("Name", None),
                                    ("Controller", Some(90.)),
                                    ("Namespace", Some(130.)),
                                    ("Revision", Some(150.)),
                                    ("Status", Some(110.)),
                                ],
                            ))
                        },
                    )
                    .child(self.render_list(cx)),
            )
            .children(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::{gitops_status, line, matching, short_revision};
    use k8s_app::present::Tone;
    use k8s_ops::gitops::GitOpsEntry;

    #[test]
    fn the_status_puts_failing_first_then_unsettled_then_drift() {
        let lines = |rows: &[(&str, Option<&str>, Option<&str>)]| -> Vec<super::Line> {
            rows.iter()
                .enumerate()
                .map(|(i, (health, applied, target))| {
                    line(entry(&format!("a{i}"), health, *applied, *target), 120)
                })
                .collect()
        };
        let (tone, label, _) = gitops_status(&lines(&[("ok", None, None), ("ok", None, None)]));
        assert_eq!((tone, label.as_str()), (Tone::Ok, "All synced"));
        let (tone, label, _) = gitops_status(&lines(&[("pending", None, None)]));
        assert_eq!((tone, label.as_str()), (Tone::Warn, "1 not settled"));
        let (tone, label, detail) = gitops_status(&lines(&[
            ("error", None, None),
            ("ok", None, None),
            ("ok", None, None),
        ]));
        assert_eq!(
            (tone, label.as_str(), detail.as_str()),
            (Tone::Error, "1 failing", "of 3")
        );
    }

    fn entry(name: &str, health: &str, applied: Option<&str>, target: Option<&str>) -> GitOpsEntry {
        GitOpsEntry {
            controller: "argocd".into(),
            kind: "Application".into(),
            resource: "argoproj.io/v1alpha1/applications".into(),
            namespace: Some("argocd".into()),
            name: name.into(),
            source: Some("https://git.example/app.git".into()),
            path: Some("deploy".into()),
            target_revision: target.map(Into::into),
            applied_revision: applied.map(Into::into),
            status: "Synced".into(),
            health: health.into(),
            message: None,
            last_sync: Some("1970-01-01T00:01:00Z".into()),
            suspended: false,
            reconcilable: true,
        }
    }

    #[test]
    fn revisions_are_short_enough_to_read() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(short_revision(hash), "0123456");
        assert_eq!(short_revision("main"), "main");
        assert_eq!(short_revision(&format!("main@sha1:{hash}")), "main@0123456");
        assert_eq!(short_revision("v1.2.3"), "v1.2.3", "a tag stays whole");
    }

    #[test]
    fn a_branch_applying_a_commit_is_normal_but_a_pinned_commit_that_differs_is_drift() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let tracking = line(entry("a", "ok", Some(hash), Some("main")), 120);
        assert!(
            !tracking.drifted,
            "tracking main and applying a commit is the usual case"
        );
        assert_eq!(&*tracking.revision, "0123456  ← main");

        let pinned = line(
            entry(
                "b",
                "ok",
                Some(hash),
                Some("fedcba9876543210fedcba9876543210fedcba98"),
            ),
            120,
        );
        assert!(pinned.drifted);

        let same = line(entry("c", "ok", Some(hash), Some(hash)), 120);
        assert!(!same.drifted);
        assert_eq!(&*same.revision, "0123456");
    }

    #[test]
    fn lines_carry_a_tone_an_age_and_a_source_label() {
        let broken = line(entry("a", "error", None, Some("main")), 120);
        assert_eq!(broken.tone, Some(k8s_app::present::Tone::Error));
        assert_eq!(&*broken.age, "1m0s");
        assert_eq!(&*broken.source, "https://git.example/app.git · deploy");
        assert_eq!(&*broken.controller, "Argo CD");
        assert_eq!(line(entry("b", "unknown", None, None), 0).tone, None);
    }

    #[test]
    fn the_filter_matches_name_namespace_status_controller_or_source() {
        let lines = vec![
            line(entry("shop", "ok", None, None), 0),
            line(entry("blog", "error", None, None), 0),
        ];
        assert_eq!(matching(&lines, "shop"), [0]);
        assert_eq!(matching(&lines, "ARGO"), [0, 1]);
        assert_eq!(matching(&lines, "nothing"), Vec::<usize>::new());
    }
}
