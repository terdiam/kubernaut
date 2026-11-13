//! The YAML tab: view the object, edit it, preview the change, apply it.
//!
//! Nothing is applied that was not previewed: Apply is enabled only for the
//! exact text a server-side dry run has just been shown for.

use std::{ops::Range, sync::Arc};

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Editor, EditorState, InputEvent},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::AppCore;
use k8s_core::objects;
use k8s_ops::apply::{self, ApplyOutcome, DiffResult, EditRequest, FieldConflict};
use tokio::runtime::Handle;

use crate::drawer::Target;

const DIFF_ROW_HEIGHT: Pixels = px(18.);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

struct DiffLine {
    kind: DiffKind,
    text: SharedString,
}

/// Classify the lines of a unified diff for colouring.
fn diff_lines(unified: &str) -> Vec<DiffLine> {
    unified
        .lines()
        .map(|line| {
            let kind = if line.starts_with("+++") || line.starts_with("---") {
                DiffKind::Header
            } else if line.starts_with("@@") {
                DiffKind::Hunk
            } else if line.starts_with('+') {
                DiffKind::Added
            } else if line.starts_with('-') {
                DiffKind::Removed
            } else {
                DiffKind::Context
            };
            DiffLine {
                kind,
                text: line.into(),
            }
        })
        .collect()
}

/// A dry run for one exact text and one setting of "take ownership".
struct Previewed {
    yaml: SharedString,
    force: bool,
    changed: bool,
    diff: Vec<DiffLine>,
    conflicts: Vec<FieldConflict>,
}

/// Apply is allowed only for text a dry run has been shown for, that would
/// change something, and that no other field manager contests.
fn can_apply(current: &str, previewed: Option<&Previewed>) -> bool {
    previewed.is_some_and(|p| *p.yaml == *current && p.changed && p.conflicts.is_empty())
}

enum Load {
    Loading,
    Ready,
    Failed(SharedString),
}

enum Notice {
    Info(SharedString),
    Error(SharedString),
}

pub struct YamlView {
    core: Arc<AppCore>,
    runtime: Handle,
    target: Target,
    editor: Entity<EditorState>,
    /// The text as the cluster last returned it, to tell an edit from none.
    original: SharedString,
    load: Load,
    previewed: Option<Previewed>,
    /// Take fields from other managers. Off by default: a forced apply can
    /// undo a controller's work.
    force: bool,
    busy: Option<&'static str>,
    notice: Option<Notice>,
    diff_scroll: UniformListScrollHandle,
    _edit: Subscription,
}

impl YamlView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
        });
        let edit = cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_edit(cx);
            }
        });

        let task = runtime.spawn(fetch_yaml(Arc::clone(core), target.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(text) => {
                    this.original = text.as_str().into();
                    this.editor
                        .update(cx, |editor, cx| editor.set_value(text, window, cx));
                    this.load = Load::Ready;
                    cx.notify();
                }
                Err(message) => {
                    this.load = Load::Failed(message.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();

        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            target: target.clone(),
            editor,
            original: SharedString::default(),
            load: Load::Loading,
            previewed: None,
            force: false,
            busy: None,
            notice: None,
            diff_scroll: UniformListScrollHandle::new(),
            _edit: edit,
        }
    }

    fn text(&self, cx: &App) -> SharedString {
        self.editor.read(cx).value()
    }

    fn is_modified(&self, cx: &App) -> bool {
        *self.text(cx) != *self.original
    }

    /// An edit makes any preview stale, so it is dropped rather than left
    /// describing text that is no longer there.
    fn on_edit(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if self.previewed.as_ref().is_some_and(|p| *p.yaml != *text) {
            self.previewed = None;
        }
        cx.notify();
    }

    fn request(&self, yaml: &str, force: bool) -> EditRequest {
        EditRequest {
            resource: self.target.resource.to_string(),
            namespace: self.target.namespace.as_ref().map(ToString::to_string),
            name: self.target.name.to_string(),
            yaml: yaml.to_string(),
            force,
        }
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || !matches!(self.load, Load::Ready) {
            return;
        }
        let yaml = self.text(cx);
        let force = self.force;
        self.busy = Some("Previewing…");
        self.notice = None;
        cx.notify();

        let request = self.request(&yaml, force);
        let (core, cluster) = (Arc::clone(&self.core), self.target.cluster.clone());
        let task = self.runtime.spawn(async move {
            let handle = core
                .clusters
                .require(&cluster)
                .map_err(|err| err.to_string())?;
            apply::preview(&handle, &request)
                .await
                .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_preview(yaml, force, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_preview(
        &mut self,
        yaml: SharedString,
        force: bool,
        result: Result<DiffResult, String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        if *self.text(cx) != *yaml {
            // Edited while the dry run was out: it describes other text.
            self.notice = Some(Notice::Info("Edited since the preview started.".into()));
            cx.notify();
            return;
        }
        match result {
            Ok(diff) => {
                self.notice = if !diff.conflicts.is_empty() {
                    Some(Notice::Error(
                        format!(
                            "{} field(s) are owned by another manager. Take ownership to override.",
                            diff.conflicts.len()
                        )
                        .into(),
                    ))
                } else if !diff.changed {
                    Some(Notice::Info("Nothing would change.".into()))
                } else {
                    None
                };
                self.previewed = Some(Previewed {
                    yaml,
                    force,
                    changed: diff.changed,
                    diff: diff_lines(&diff.unified),
                    conflicts: diff.conflicts,
                });
            }
            Err(message) => {
                self.previewed = None;
                self.notice = Some(Notice::Error(message.into()));
            }
        }
        cx.notify();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let yaml = self.text(cx);
        if self.busy.is_some() || !can_apply(&yaml, self.previewed.as_ref()) {
            return;
        }
        // Protected contexts refuse every change, not just destructive ones.
        if let Err(message) = self.core.ensure_writable(&self.target.cluster) {
            self.notice = Some(Notice::Error(message.into()));
            cx.notify();
            return;
        }
        let force = self.previewed.as_ref().is_some_and(|p| p.force);
        self.busy = Some("Applying…");
        self.notice = None;
        cx.notify();

        let request = self.request(&yaml, force);
        let (core, cluster) = (Arc::clone(&self.core), self.target.cluster.clone());
        let task = self.runtime.spawn(async move {
            let handle = core
                .clusters
                .require(&cluster)
                .map_err(|err| err.to_string())?;
            apply::apply(&handle, &request)
                .await
                .map_err(|err| err.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_apply(yaml, force, result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_apply(
        &mut self,
        yaml: SharedString,
        force: bool,
        result: Result<ApplyOutcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            Ok(ApplyOutcome::Applied {
                yaml: applied,
                resource_version,
            }) => {
                // Show what the cluster now holds, so the editor is not left
                // describing the object as it was.
                self.original = applied.as_str().into();
                self.previewed = None;
                self.force = false;
                self.editor
                    .update(cx, |editor, cx| editor.set_value(applied, window, cx));
                self.notice = Some(Notice::Info(
                    format!(
                        "Applied. resourceVersion {}",
                        resource_version.as_deref().unwrap_or("unknown")
                    )
                    .into(),
                ));
            }
            Ok(ApplyOutcome::Conflict { conflicts }) => {
                // The cluster changed under the preview; show who owns what.
                self.notice = Some(Notice::Error(
                    "Another field manager owns some of these fields. Nothing was applied.".into(),
                ));
                self.previewed = Some(Previewed {
                    yaml,
                    force,
                    changed: true,
                    diff: Vec::new(),
                    conflicts,
                });
            }
            Err(message) => self.notice = Some(Notice::Error(message.into())),
        }
        cx.notify();
    }

    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let original = self.original.clone();
        self.editor
            .update(cx, |editor, cx| editor.set_value(original, window, cx));
        self.previewed = None;
        self.force = false;
        self.notice = None;
        cx.notify();
    }

    fn toggle_force(&mut self, cx: &mut Context<Self>) {
        self.force = !self.force;
        // A preview was made under the other setting.
        self.previewed = None;
        cx.notify();
    }

    fn chip(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        active: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .border_1()
            .border_color(theme.border)
            .when(active, |chip| chip.bg(theme.secondary).font_semibold())
            .when(enabled, |chip| chip.cursor_pointer())
            .when(!enabled, |chip| {
                chip.opacity(0.4).text_color(theme.muted_foreground)
            })
            .child(label)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let ready = matches!(self.load, Load::Ready) && self.busy.is_none();
        let modified = self.is_modified(cx);
        let applicable = ready && can_apply(&self.text(cx), self.previewed.as_ref());
        let contested = self
            .previewed
            .as_ref()
            .is_some_and(|p| !p.conflicts.is_empty());

        h_flex()
            .flex_none()
            .px_3()
            .pb_2()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child({
                let chip = self.chip("preview", "Preview", ready, false, cx);
                if ready {
                    chip.on_click(cx.listener(|this, _, _, cx| this.preview(cx)))
                } else {
                    chip
                }
            })
            .child({
                let chip = self.chip("apply", "Apply", applicable, false, cx);
                if applicable {
                    chip.on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)))
                } else {
                    chip
                }
            })
            .child({
                let chip = self.chip("revert", "Revert", ready && modified, false, cx);
                if ready && modified {
                    chip.on_click(cx.listener(|this, _, window, cx| this.revert(window, cx)))
                } else {
                    chip
                }
            })
            .when(contested || self.force, |bar| {
                bar.child(
                    self.chip("force", "Take ownership", ready, self.force, cx)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_force(cx))),
                )
            })
            .when(modified, |bar| {
                bar.child(div().text_xs().text_color(warning).child("Modified"))
            })
            .when_some(self.busy, |bar, text| {
                bar.child(div().text_xs().text_color(muted).child(text))
            })
    }

    fn render_changes(&self, previewed: &Previewed, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, danger) = (theme.border, theme.muted_foreground, theme.danger);

        v_flex()
            .flex_none()
            .h(px(220.))
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(if previewed.conflicts.is_empty() {
                        "Changes"
                    } else {
                        "Conflicts"
                    }),
            )
            .children(previewed.conflicts.iter().take(8).map(|conflict| {
                div()
                    .px_3()
                    .text_xs()
                    .text_color(danger)
                    .truncate()
                    .child(format!("{} owns {}", conflict.manager, conflict.field))
            }))
            .when(!previewed.diff.is_empty(), |panel| {
                panel.child(
                    uniform_list(
                        "diff",
                        previewed.diff.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            let theme = cx.theme();
                            let (success, danger, muted, info) = (
                                theme.success,
                                theme.danger,
                                theme.muted_foreground,
                                theme.info,
                            );
                            let mono = theme.mono_font_family.clone();
                            let Some(previewed) = &this.previewed else {
                                return Vec::new();
                            };
                            previewed
                                .diff
                                .get(range.clone())
                                .unwrap_or_default()
                                .iter()
                                .zip(range)
                                .map(|(line, index)| {
                                    let color = match line.kind {
                                        DiffKind::Added => Some(success),
                                        DiffKind::Removed => Some(danger),
                                        DiffKind::Hunk => Some(info),
                                        DiffKind::Header => Some(muted),
                                        DiffKind::Context => None,
                                    };
                                    div()
                                        .id(("diff-line", index))
                                        .w_full()
                                        .h(DIFF_ROW_HEIGHT)
                                        .px_3()
                                        .text_xs()
                                        .font_family(mono.clone())
                                        .truncate()
                                        .when_some(color, |row, color| row.text_color(color))
                                        .child(line.text.clone())
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.diff_scroll)
                    .flex_1(),
                )
            })
    }
}

impl Render for YamlView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);

        let body = match &self.load {
            Load::Loading => div()
                .p_4()
                .text_sm()
                .text_color(muted)
                .child("Loading…")
                .into_any_element(),
            Load::Failed(error) => div()
                .p_4()
                .text_sm()
                .text_color(danger)
                .child(error.clone())
                .into_any_element(),
            Load::Ready => div()
                .flex_1()
                .min_h_0()
                .child(Editor::new(&self.editor).bordered(false).h(relative(1.)))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .child(self.render_toolbar(cx))
            .when_some(self.notice.as_ref(), |column, notice| {
                let (text, color) = match notice {
                    Notice::Info(text) => (text.clone(), muted),
                    Notice::Error(text) => (text.clone(), danger),
                };
                column.child(div().px_3().pb_2().text_xs().text_color(color).child(text))
            })
            .child(body)
            .when_some(self.previewed.as_ref(), |column, previewed| {
                column.child(self.render_changes(previewed, cx))
            })
    }
}

async fn fetch_yaml(core: Arc<AppCore>, target: Target) -> Result<String, String> {
    let handle = core
        .clusters
        .require(&target.cluster)
        .map_err(|err| err.to_string())?;
    let object = objects::get(
        &handle,
        &target.resource,
        target.namespace.as_deref(),
        &target.name,
    )
    .await
    .map_err(|err| err.to_string())?;
    objects::to_yaml(&object, false).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{DiffKind, Previewed, can_apply, diff_lines};
    use k8s_ops::apply::FieldConflict;

    fn previewed(yaml: &str, changed: bool, conflicts: Vec<FieldConflict>) -> Previewed {
        Previewed {
            yaml: yaml.into(),
            force: false,
            changed,
            diff: Vec::new(),
            conflicts,
        }
    }

    #[test]
    fn unified_diff_lines_are_classified_for_colouring() {
        let diff = "--- live\n+++ proposed\n@@ -1,3 +1,3 @@\n a: 1\n-b: 2\n+b: 3\n";
        let kinds: Vec<_> = diff_lines(diff).iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                DiffKind::Header,
                DiffKind::Header,
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Removed,
                DiffKind::Added,
            ]
        );
        assert!(diff_lines("").is_empty());
    }

    #[test]
    fn apply_needs_a_preview_of_exactly_this_text_that_changes_something() {
        let text = "a: 1";
        assert!(!can_apply(text, None), "never previewed");
        assert!(can_apply(text, Some(&previewed(text, true, Vec::new()))));
        assert!(
            !can_apply("a: 2", Some(&previewed(text, true, Vec::new()))),
            "edited since the preview"
        );
        assert!(
            !can_apply(text, Some(&previewed(text, false, Vec::new()))),
            "nothing would change"
        );
    }

    #[test]
    fn a_contested_field_blocks_apply_until_ownership_is_taken() {
        let conflict = FieldConflict {
            manager: "rancher".into(),
            field: ".spec.replicas".into(),
        };
        assert!(!can_apply(
            "a: 1",
            Some(&previewed("a: 1", true, vec![conflict]))
        ));
    }
}
