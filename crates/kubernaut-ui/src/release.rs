//! The detail pane for one Helm release: history, values, manifest, notes, and
//! the two operations that change it, rollback and uninstall.

use std::{cmp::Reverse, ops::Range, sync::Arc};

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Editor, EditorState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, helm_ops, present, present::Tone};
use k8s_helm::{ReleaseDetail, ReleaseRevision, store};
use tokio::runtime::Handle;

use crate::ui::{Palette, chip, message, tone_color};

const ROW_HEIGHT: Pixels = px(28.);

/// What the pane tells the list that owns it.
pub enum ReleaseEvent {
    Close,
    /// An operation changed the release, so the list is out of date.
    Changed,
}

/// Helm's own statuses, mapped to the tones the rest of the app uses. Only the
/// ones that call for attention or confirm health get a colour.
pub fn status_tone(status: &str) -> Option<Tone> {
    match status {
        "deployed" => Some(Tone::Ok),
        "failed" => Some(Tone::Error),
        "uninstalling" => Some(Tone::Pending),
        other if other.starts_with("pending") => Some(Tone::Pending),
        _ => None,
    }
}

/// One revision as the history list draws it. Labels are built once, at load.
struct RevisionLine {
    revision: i64,
    label: SharedString,
    status: SharedString,
    tone: Option<Tone>,
    chart_version: SharedString,
    age: SharedString,
    description: SharedString,
}

/// Newest first.
fn revision_lines(history: &[ReleaseRevision], now: i64) -> Vec<RevisionLine> {
    let mut sorted: Vec<&ReleaseRevision> = history.iter().collect();
    sorted.sort_by_key(|revision| Reverse(revision.revision));

    let mut age = String::new();
    sorted
        .into_iter()
        .map(|revision| {
            age.clear();
            if let Some(updated) = revision
                .updated
                .as_deref()
                .and_then(present::parse_timestamp)
            {
                present::write_age(&mut age, now - updated);
            }
            RevisionLine {
                revision: revision.revision,
                label: format!("#{}", revision.revision).into(),
                status: revision.status.as_str().into(),
                tone: status_tone(&revision.status),
                chart_version: revision.chart_version.as_str().into(),
                age: age.as_str().into(),
                description: revision.description.as_deref().unwrap_or_default().into(),
            }
        })
        .collect()
}

/// Rolling back to the revision already live would only add a revision.
fn can_roll_back_to(revision: i64, current: i64) -> bool {
    revision != current
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    History,
    Values,
    Manifest,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Values {
    Supplied,
    Effective,
}

/// An operation waiting for the user to say yes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Rollback(i64),
    Uninstall,
}

struct Loaded {
    history: Vec<RevisionLine>,
    supplied: SharedString,
    effective: SharedString,
    manifest: SharedString,
    notes: SharedString,
}

impl Loaded {
    fn new(detail: ReleaseDetail, history: &[ReleaseRevision]) -> Self {
        Self {
            history: revision_lines(history, present::now()),
            supplied: if detail.user_values.trim().is_empty() {
                "# This release used the chart's default values.\n".into()
            } else {
                detail.user_values.into()
            },
            effective: detail.effective_values.into(),
            manifest: detail.manifest.into(),
            notes: detail.notes.into(),
        }
    }

    fn values(&self, mode: Values) -> &SharedString {
        match mode {
            Values::Supplied => &self.supplied,
            Values::Effective => &self.effective,
        }
    }
}

enum Load {
    Loading,
    Ready(Loaded),
    Failed(SharedString),
}

enum Notice {
    Info(SharedString),
    Error(SharedString),
}

pub struct ReleaseView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    namespace: SharedString,
    name: SharedString,
    /// The live revision, from the list. Refreshed with the history.
    current: i64,
    chart: SharedString,
    load: Load,
    tab: Tab,
    values: Values,
    /// Created when their tab is first opened: a manifest can be large, and an
    /// unopened tab should cost nothing.
    values_editor: Option<Entity<EditorState>>,
    manifest_editor: Option<Entity<EditorState>>,
    notes_editor: Option<Entity<EditorState>>,
    confirm: Option<Confirm>,
    busy: Option<&'static str>,
    notice: Option<Notice>,
    history_scroll: UniformListScrollHandle,
}

impl EventEmitter<ReleaseEvent> for ReleaseView {}

impl ReleaseView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        release: &k8s_helm::Release,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            namespace: release.namespace.as_str().into(),
            name: release.name.as_str().into(),
            current: release.revision,
            chart: format!("{} {}", release.chart, release.chart_version).into(),
            load: Load::Loading,
            tab: Tab::History,
            values: Values::Supplied,
            values_editor: None,
            manifest_editor: None,
            notes_editor: None,
            confirm: None,
            busy: None,
            notice: None,
            history_scroll: UniformListScrollHandle::new(),
        };
        view.reload(window, cx);
        view
    }

    /// Fetch the release again and replace what is shown, editors included.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let (namespace, name) = (self.namespace.clone(), self.name.clone());
        let task = self.runtime.spawn(async move {
            let handle = core
                .clusters
                .require(&cluster)
                .map_err(|err| err.to_string())?;
            let (detail, history) = tokio::join!(
                store::detail(&handle, &namespace, &name, None),
                store::history(&handle, &namespace, &name),
            );
            Ok::<_, String>((
                detail.map_err(|err| err.to_string())?,
                history.map_err(|err| err.to_string())?,
            ))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_load(result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_load(
        &mut self,
        result: Result<(ReleaseDetail, Vec<ReleaseRevision>), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((detail, history)) => {
                self.current = detail.release.revision;
                let loaded = Loaded::new(detail, &history);
                // Editors that exist hold the old text.
                let values = loaded.values(self.values).clone();
                for (editor, text) in [
                    (&self.values_editor, values),
                    (&self.manifest_editor, loaded.manifest.clone()),
                    (&self.notes_editor, loaded.notes.clone()),
                ] {
                    if let Some(editor) = editor {
                        editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
                    }
                }
                self.load = Load::Ready(loaded);
            }
            Err(message) => self.load = Load::Failed(message.into()),
        }
        cx.notify();
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        if let Load::Ready(loaded) = &self.load {
            match tab {
                Tab::Values if self.values_editor.is_none() => {
                    let text = loaded.values(self.values).clone();
                    self.values_editor = Some(readonly_editor(text, true, window, cx));
                }
                Tab::Manifest if self.manifest_editor.is_none() => {
                    self.manifest_editor =
                        Some(readonly_editor(loaded.manifest.clone(), true, window, cx));
                }
                Tab::Notes if self.notes_editor.is_none() => {
                    self.notes_editor =
                        Some(readonly_editor(loaded.notes.clone(), false, window, cx));
                }
                _ => {}
            }
        }
        cx.notify();
    }

    fn show_values(&mut self, mode: Values, window: &mut Window, cx: &mut Context<Self>) {
        self.values = mode;
        if let (Load::Ready(loaded), Some(editor)) = (&self.load, &self.values_editor) {
            let text = loaded.values(mode).clone();
            editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        cx.notify();
    }

    fn ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        if self.busy.is_none() {
            self.confirm = Some(confirm);
            self.notice = None;
            cx.notify();
        }
    }

    fn run(&mut self, confirm: Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.confirm = None;
        self.notice = None;
        self.busy = Some(match confirm {
            Confirm::Rollback(_) => "Rolling back…",
            Confirm::Uninstall => "Uninstalling…",
        });
        cx.notify();

        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let (namespace, name) = (self.namespace.clone(), self.name.clone());
        let task = self.runtime.spawn(async move {
            match confirm {
                Confirm::Rollback(revision) => {
                    helm_ops::rollback(&core, &cluster, &namespace, &name, revision).await
                }
                Confirm::Uninstall => helm_ops::uninstall(&core, &cluster, &namespace, &name).await,
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_run(confirm, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_run(
        &mut self,
        confirm: Confirm,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            Ok(_) => {
                cx.emit(ReleaseEvent::Changed);
                match confirm {
                    Confirm::Uninstall => cx.emit(ReleaseEvent::Close),
                    Confirm::Rollback(revision) => {
                        self.notice =
                            Some(Notice::Info(format!("Rolled back to #{revision}.").into()));
                        self.reload(window, cx);
                    }
                }
            }
            Err(message) => self.notice = Some(Notice::Error(message.into())),
        }
        cx.notify();
    }

    fn render_tab(
        &self,
        tab: Tab,
        label: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.tab == tab;
        let secondary = cx.theme().secondary;
        div()
            .id(label)
            .px_3()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .when(active, |tab| tab.bg(secondary).font_semibold())
            .when(!active, |tab| tab.text_color(palette.muted))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
    }

    fn render_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let idle = self.busy.is_none();
        let ask_uninstall = cx.listener(|this, _, _, cx| this.ask(Confirm::Uninstall, cx));

        let prompt = self.confirm.map(|confirm| {
            let text: SharedString = match confirm {
                Confirm::Rollback(revision) => format!(
                    "Roll {} back to #{revision}? The cluster is changed.",
                    self.name
                )
                .into(),
                Confirm::Uninstall => format!(
                    "Uninstall {}? Everything it installed is deleted.",
                    self.name
                )
                .into(),
            };
            let go = cx.listener(move |this, _, window, cx| this.run(confirm, window, cx));
            let cancel = cx.listener(|this, _, _, cx| {
                this.confirm = None;
                cx.notify();
            });
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(div().text_xs().text_color(palette.danger).child(text))
                .child(chip("confirm", "Confirm", true, false, cx.theme()).on_click(go))
                .child(chip("cancel", "Cancel", true, false, cx.theme()).on_click(cancel))
        });

        h_flex()
            .flex_none()
            .px_3()
            .pb_2()
            .gap_2()
            .items_center()
            .flex_wrap()
            .when(prompt.is_none(), |bar| {
                bar.child(
                    chip("uninstall", "Uninstall", idle, false, cx.theme())
                        .when(idle, |chip| chip.on_click(ask_uninstall)),
                )
            })
            .children(prompt)
            .when_some(self.busy, |bar, text| {
                bar.child(div().text_xs().text_color(palette.muted).child(text))
            })
    }

    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let Load::Ready(loaded) = &self.load else {
            return div().into_any_element();
        };
        uniform_list(
            "release-history",
            loaded.history.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let Load::Ready(loaded) = &this.load else {
                    return Vec::new();
                };
                let idle = this.busy.is_none();
                let muted = cx.theme().muted_foreground;
                loaded
                    .history
                    .get(range.clone())
                    .unwrap_or_default()
                    .iter()
                    .zip(range)
                    .map(|(line, index)| {
                        let revision = line.revision;
                        let enabled = idle && can_roll_back_to(revision, this.current);
                        let rollback = cx.listener(move |this, _, _, cx| {
                            this.ask(Confirm::Rollback(revision), cx);
                        });
                        let color = line.tone.map(|tone| tone_color(tone, cx.theme()));
                        h_flex()
                            .id(("revision", index))
                            .w_full()
                            .h(ROW_HEIGHT)
                            .px_3()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .child(div().w(px(32.)).flex_none().child(line.label.clone()))
                            .child(
                                div()
                                    .w(px(92.))
                                    .flex_none()
                                    .truncate()
                                    .when_some(color, |cell, color| cell.text_color(color))
                                    .child(line.status.clone()),
                            )
                            .child(
                                div()
                                    .w(px(84.))
                                    .flex_none()
                                    .truncate()
                                    .child(line.chart_version.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(muted)
                                    .child(line.description.clone()),
                            )
                            .child(
                                div()
                                    .w(px(40.))
                                    .flex_none()
                                    .text_color(muted)
                                    .child(line.age.clone()),
                            )
                            .child(
                                chip(("rollback", index), "Roll back", enabled, false, cx.theme())
                                    .flex_none()
                                    .when(enabled, |chip| chip.on_click(rollback)),
                            )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.history_scroll)
        .flex_1()
        .into_any_element()
    }

    fn render_values(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let supplied = cx.listener(|this, _, window, cx| {
            this.show_values(Values::Supplied, window, cx);
        });
        let effective = cx.listener(|this, _, window, cx| {
            this.show_values(Values::Effective, window, cx);
        });
        let toggle = h_flex()
            .flex_none()
            .px_3()
            .pb_2()
            .gap_2()
            .child(
                chip(
                    "supplied",
                    "Supplied",
                    true,
                    self.values == Values::Supplied,
                    cx.theme(),
                )
                .on_click(supplied),
            )
            .child(
                chip(
                    "effective",
                    "With defaults",
                    true,
                    self.values == Values::Effective,
                    cx.theme(),
                )
                .on_click(effective),
            );
        match &self.values_editor {
            Some(editor) => v_flex()
                .size_full()
                .child(toggle)
                .child(div().flex_1().min_h_0().child(editor_view(editor)))
                .into_any_element(),
            None => message("Loading…", palette.muted).into_any_element(),
        }
    }
}

impl Render for ReleaseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let (background, error_notice) = (cx.theme().background, palette.danger);

        let body = match &self.load {
            Load::Loading => message("Loading…", palette.muted).into_any_element(),
            Load::Failed(error) => message(error.clone(), palette.danger).into_any_element(),
            Load::Ready(_) => match self.tab {
                Tab::History => self.render_history(cx),
                Tab::Values => self.render_values(palette, cx),
                Tab::Manifest => self.editor_or_loading(&self.manifest_editor, palette),
                Tab::Notes => self.editor_or_loading(&self.notes_editor, palette),
            },
        };

        v_flex()
            .w(px(560.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(palette.border)
            .child(
                h_flex()
                    .h(px(52.))
                    .flex_none()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().truncate().child(self.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .truncate()
                                    .child(format!("{} · {}", self.namespace, self.chart)),
                            ),
                    )
                    .child(
                        chip("close-release", "Close", true, false, cx.theme())
                            .text_sm()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ReleaseEvent::Close))),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(self.render_tab(Tab::History, "History", palette, cx))
                    .child(self.render_tab(Tab::Values, "Values", palette, cx))
                    .child(self.render_tab(Tab::Manifest, "Manifest", palette, cx))
                    .child(self.render_tab(Tab::Notes, "Notes", palette, cx)),
            )
            .child(self.render_actions(cx))
            .when_some(self.notice.as_ref(), |column, notice| {
                let (text, color) = match notice {
                    Notice::Info(text) => (text.clone(), palette.muted),
                    Notice::Error(text) => (text.clone(), error_notice),
                };
                column.child(div().px_3().pb_2().text_xs().text_color(color).child(text))
            })
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    }
}

impl ReleaseView {
    fn editor_or_loading(
        &self,
        editor: &Option<Entity<EditorState>>,
        palette: Palette,
    ) -> AnyElement {
        match editor {
            Some(editor) => editor_view(editor).into_any_element(),
            None => message("Loading…", palette.muted).into_any_element(),
        }
    }
}

fn editor_view(editor: &Entity<EditorState>) -> Editor {
    Editor::new(editor)
        .readonly(true)
        .bordered(false)
        .h(relative(1.))
}

/// A read-only editor holding `text`. `yaml` turns on highlighting; notes are
/// plain text.
fn readonly_editor(
    text: SharedString,
    yaml: bool,
    window: &mut Window,
    cx: &mut Context<ReleaseView>,
) -> Entity<EditorState> {
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx);
        if yaml {
            state = state.language("yaml");
        }
        let mut state = state.line_number(yaml);
        state.set_value(text, window, cx);
        state
    })
}

#[cfg(test)]
mod tests {
    use super::{can_roll_back_to, revision_lines, status_tone};
    use k8s_app::present::Tone;
    use k8s_helm::ReleaseRevision;

    fn revision(number: i64, status: &str, updated: Option<&str>) -> ReleaseRevision {
        ReleaseRevision {
            revision: number,
            status: status.into(),
            chart_version: "1.2.3".into(),
            app_version: None,
            updated: updated.map(Into::into),
            description: Some(format!("revision {number}")),
        }
    }

    #[test]
    fn helm_statuses_map_to_tones() {
        assert_eq!(status_tone("deployed"), Some(Tone::Ok));
        assert_eq!(status_tone("failed"), Some(Tone::Error));
        assert_eq!(status_tone("pending-upgrade"), Some(Tone::Pending));
        assert_eq!(status_tone("pending-install"), Some(Tone::Pending));
        assert_eq!(status_tone("uninstalling"), Some(Tone::Pending));
        assert_eq!(status_tone("superseded"), None, "history, not a problem");
    }

    #[test]
    fn history_is_newest_first_with_ages() {
        let history = [
            revision(1, "superseded", Some("1970-01-01T00:01:00Z")),
            revision(3, "deployed", Some("1970-01-01T00:10:00Z")),
            revision(2, "failed", None),
        ];
        let lines = revision_lines(&history, 660);

        let order: Vec<_> = lines.iter().map(|line| line.revision).collect();
        assert_eq!(order, [3, 2, 1]);
        assert_eq!(&*lines[0].label, "#3");
        assert_eq!(&*lines[0].age, "1m0s");
        assert_eq!(&*lines[1].age, "", "no timestamp, no age");
        assert_eq!(lines[1].tone, Some(Tone::Error));
    }

    #[test]
    fn the_live_revision_cannot_be_rolled_back_to() {
        assert!(!can_roll_back_to(4, 4));
        assert!(can_roll_back_to(3, 4));
    }
}
