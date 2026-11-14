//! Create: write, paste or import a manifest and apply it.
//!
//! The same rule as editing an object: nothing is applied that was not
//! previewed. Apply is enabled only for the exact text, namespace and
//! ownership setting that a dry run has just been shown for, and the preview
//! says per document whether it would create, change or leave alone.

use std::{ops::Range, path::PathBuf, sync::Arc};

use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Theme, WindowExt,
        button::ButtonVariant,
        h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, manifest_ops, templates};
use k8s_ops::manifest::{DocResult, ManifestPlan, PruneRequest};
use tokio::runtime::Handle;

use crate::{
    diff::{self, DiffLine},
    form::{FormContext, FormEvent, FormView},
    ui::{Emphasis, button_icon, chip, page_title, pill, toast_error, toast_success},
    validation::ValidationView,
};

/// Larger than any manifest a person writes; a bigger file is more likely the
/// wrong one than a real manifest.
const MAX_IMPORT_BYTES: u64 = 5 * 1024 * 1024;

/// A dry run for one exact text, namespace and ownership setting.
struct Previewed {
    yaml: SharedString,
    namespace: String,
    force: bool,
    /// The label selector the plan was made to prune with, empty for no prune.
    prune: String,
    plan: ManifestPlan,
    /// One parsed diff per document, built once.
    diffs: Vec<Vec<DiffLine>>,
}

/// Whether a plan can be applied as it stands: nothing is an error or a
/// contested field, and something would actually change or be pruned.
fn applicable(plan: &ManifestPlan) -> bool {
    !plan.docs.is_empty()
        && plan
            .docs
            .iter()
            .all(|doc| doc.action != "error" && doc.action != "conflict")
        && plan
            .docs
            .iter()
            .any(|doc| doc.action == "create" || doc.action == "update")
        || !plan.prune.is_empty()
}

fn can_apply(
    yaml: &str,
    namespace: &str,
    force: bool,
    prune: &str,
    previewed: Option<&Previewed>,
) -> bool {
    previewed.is_some_and(|p| {
        *p.yaml == *yaml
            && p.namespace == namespace
            && p.force == force
            && p.prune == prune
            && applicable(&p.plan)
    })
}

/// A sentence for the toast after an apply.
fn summary(results: &[DocResult]) -> String {
    let count = |status: &str| results.iter().filter(|r| r.status == status).count();
    let parts: Vec<String> = [
        ("created", count("created")),
        ("configured", count("configured")),
        ("unchanged", count("unchanged")),
        ("pruned", count("pruned")),
        ("failed", count("error") + count("conflict")),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .map(|(label, n)| format!("{n} {label}"))
    .collect();
    if parts.is_empty() {
        "Nothing applied".to_string()
    } else {
        parts.join(", ")
    }
}

/// One line for the confirmation: `ConfigMap app/old`.
fn describe_target(target: &k8s_ops::manifest::PruneTarget) -> String {
    match &target.namespace {
        Some(namespace) => format!("{} {namespace}/{}", target.kind, target.name),
        None => format!("{} {}", target.kind, target.name),
    }
}

fn status_color(status: &str, theme: &Theme) -> Hsla {
    match status {
        "create" | "created" => theme.success,
        "update" | "configured" => theme.info,
        "delete" | "pruned" => theme.danger,
        "conflict" => theme.warning,
        "error" => theme.danger,
        _ => theme.muted_foreground,
    }
}

enum Notice {
    Info(SharedString),
    Error(SharedString),
}

pub struct CreateView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    editor: Entity<EditorState>,
    /// Namespace for documents that name none.
    namespace: Entity<InputState>,
    force: bool,
    /// Delete what an earlier apply of this manifest made and it no longer lists.
    prune: bool,
    /// Which objects a prune may consider, as a label selector.
    prune_selector: Entity<InputState>,
    previewed: Option<Previewed>,
    results: Option<Vec<DocResult>>,
    selected: usize,
    busy: Option<&'static str>,
    notice: Option<Notice>,
    diff_scroll: UniformListScrollHandle,
    /// The form, when one is filling in the editor.
    form: Option<(Entity<FormView>, Subscription)>,
    /// What the schemas say is wrong with the text.
    validation: Entity<ValidationView>,
    /// The text the form last wrote. An editor that differs from it was edited
    /// by hand.
    last_form_yaml: SharedString,
    /// True once the YAML was edited by hand: the form no longer overwrites it.
    detached: bool,
    _edit: Subscription,
    _namespace: Subscription,
    _prune_selector: Subscription,
}

impl CreateView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        namespace: Option<&SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let start = templates::TEMPLATES
            .iter()
            .find(|template| template.id == "deployment")
            .map_or("", |template| template.yaml);
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("yaml")
                .line_number(true);
            state.set_value(start, window, cx);
            state
        });
        let edit = cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_edit(cx);
            }
        });
        let validation =
            cx.new(|cx| ValidationView::new(core, runtime, cluster.clone(), &editor, window, cx));
        validation.update(cx, |validation, cx| validation.check(cx));
        let namespace_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("from the manifest");
            if let Some(namespace) = namespace {
                state.set_value(namespace.clone(), window, cx);
            }
            state
        });
        let namespace_edit = cx.subscribe_in(
            &namespace_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_edit(cx);
                }
            },
        );
        let prune_selector = cx.new(|cx| InputState::new(window, cx).placeholder("app=web"));
        let selector_edit = cx.subscribe_in(
            &prune_selector,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_edit(cx);
                }
            },
        );
        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            editor,
            namespace: namespace_input,
            force: false,
            prune: false,
            prune_selector,
            previewed: None,
            results: None,
            selected: 0,
            busy: None,
            notice: None,
            diff_scroll: UniformListScrollHandle::new(),
            form: None,
            validation,
            last_form_yaml: SharedString::default(),
            detached: false,
            _edit: edit,
            _namespace: namespace_edit,
            _prune_selector: selector_edit,
        }
    }

    fn text(&self, cx: &App) -> SharedString {
        self.editor.read(cx).value()
    }

    /// Check text that was set from code, which the editor does not report.
    fn revalidate(&self, cx: &mut Context<Self>) {
        self.validation
            .update(cx, |validation, cx| validation.check(cx));
    }

    /// The selector a prune would use; empty when pruning is off.
    fn prune_text(&self, cx: &App) -> String {
        if self.prune {
            self.prune_selector.read(cx).value().trim().to_string()
        } else {
            String::new()
        }
    }

    fn namespace_text(&self, cx: &App) -> String {
        self.namespace.read(cx).value().trim().to_string()
    }

    /// An edit makes a preview stale, so it is dropped rather than left
    /// describing text that is no longer there.
    fn on_edit(&mut self, cx: &mut Context<Self>) {
        let (text, namespace) = (self.text(cx), self.namespace_text(cx));
        // The form's own write is not an edit; anything else is.
        if self.form.is_some() && !self.detached && *text != *self.last_form_yaml {
            self.detached = true;
        }
        if self.previewed.as_ref().is_some_and(|p| {
            *p.yaml != *text || p.namespace != namespace || p.prune != self.prune_text(cx)
        }) {
            self.previewed = None;
        }
        cx.notify();
    }

    fn load_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        // Text from a template, a file or a folder replaces the form's.
        self.form = None;
        self.detached = false;
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        self.revalidate(cx);
        self.previewed = None;
        self.results = None;
        self.notice = None;
        cx.notify();
    }

    /// Open the form for a kind. It writes the manifest into the editor as it is
    /// filled in.
    fn open_form(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let namespace = self.namespace_text(cx);
        let place = FormContext {
            core: Arc::clone(&self.core),
            runtime: self.runtime.clone(),
            cluster: self.cluster.clone(),
            namespace: (!namespace.is_empty()).then_some(namespace),
        };
        let Some(view) = FormView::create(&place, kind, window, cx) else {
            return;
        };
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &FormEvent, window, cx| match event {
                FormEvent::Changed(yaml) => this.take_form_yaml(yaml, window, cx),
            },
        );
        self.form = Some((view.clone(), subscription));
        self.detached = false;
        // The form wrote its first manifest while being made, before anyone
        // listened.
        view.update(cx, |form, cx| form.changed(cx));
        cx.notify();
    }

    /// Put what the form describes into the editor, unless it was edited by hand.
    fn take_form_yaml(&mut self, yaml: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.detached {
            return;
        }
        self.last_form_yaml = yaml.to_string().into();
        self.editor.update(cx, |editor, cx| {
            editor.set_value(yaml.to_string(), window, cx)
        });
        self.revalidate(cx);
        self.previewed = None;
        self.results = None;
        self.notice = None;
        cx.notify();
    }

    /// Let the form write the editor again, discarding the hand edits.
    fn use_form_again(&mut self, cx: &mut Context<Self>) {
        self.detached = false;
        if let Some((form, _)) = &self.form {
            form.update(cx, |form, cx| form.changed(cx));
        }
    }

    fn use_template(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(template) = templates::TEMPLATES.get(index) {
            self.load_text(template.yaml.to_string(), window, cx);
        }
    }

    /// Ask for a file or a folder, then hand what was chosen to `read`.
    fn choose(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some(if directory { "Build" } else { "Import" }.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = chosen
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            if let Some(path) = path {
                this.update_in(cx, |this, window, cx| {
                    this.read(path, directory, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn read(
        &mut self,
        path: PathBuf,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(if directory {
            "Building…"
        } else {
            "Reading…"
        });
        cx.notify();
        let (core, runtime) = (Arc::clone(&self.core), self.runtime.clone());
        let task = runtime.spawn(async move {
            if directory {
                return manifest_ops::kustomize_build(&core, &path).await;
            }
            let length = tokio::fs::metadata(&path)
                .await
                .map_err(|err| err.to_string())?
                .len();
            if length > MAX_IMPORT_BYTES {
                return Err(
                    "That file is larger than 5 MB, which is too big for a manifest.".to_string(),
                );
            }
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|err| err.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(text) => this.load_text(text, window, cx),
                    Err(reason) => {
                        toast_error(
                            window,
                            cx,
                            if directory {
                                "Could not build it"
                            } else {
                                "Could not read it"
                            },
                            reason,
                        );
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let (yaml, namespace, force) = (self.text(cx), self.namespace_text(cx), self.force);
        let prune = self.prune_text(cx);
        if self.prune && prune.is_empty() {
            self.notice = Some(Notice::Error(
                "Type a label selector, such as app=web, to say which objects a prune may delete."
                    .into(),
            ));
            cx.notify();
            return;
        }
        self.busy = Some("Previewing…");
        self.notice = None;
        self.results = None;
        cx.notify();

        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let (text, scope, selector) = (yaml.to_string(), namespace.clone(), prune.clone());
        let task = self.runtime.spawn(async move {
            let scope = (!scope.is_empty()).then_some(scope);
            let selector = (!selector.is_empty()).then_some(selector);
            manifest_ops::plan(
                &core,
                &cluster,
                &text,
                scope.as_deref(),
                force,
                selector.as_deref(),
            )
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.finish_preview(yaml, namespace, force, prune, result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_preview(
        &mut self,
        yaml: SharedString,
        namespace: String,
        force: bool,
        prune: String,
        result: Result<ManifestPlan, String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        if *self.text(cx) != *yaml
            || self.namespace_text(cx) != namespace
            || self.prune_text(cx) != prune
        {
            self.notice = Some(Notice::Info("Edited since the preview started.".into()));
            cx.notify();
            return;
        }
        match result {
            Ok(plan) => {
                self.notice = if plan.docs.iter().any(|d| d.action == "error") {
                    Some(Notice::Error(
                        "Fix the documents marked error to apply.".into(),
                    ))
                } else if plan.docs.iter().any(|d| d.action == "conflict") {
                    Some(Notice::Error(
                        "Another manager owns some fields. Take ownership to override.".into(),
                    ))
                } else if !applicable(&plan) {
                    Some(Notice::Info("Nothing would change.".into()))
                } else {
                    None
                };
                // Open on the first document that needs attention.
                self.selected = plan
                    .docs
                    .iter()
                    .position(|d| d.action == "error" || d.action == "conflict")
                    .unwrap_or(0);
                let diffs = plan.docs.iter().map(|d| diff::parse(&d.unified)).collect();
                self.previewed = Some(Previewed {
                    yaml,
                    namespace,
                    force,
                    prune,
                    plan,
                    diffs,
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
        let (yaml, namespace, prune) =
            (self.text(cx), self.namespace_text(cx), self.prune_text(cx));
        if self.busy.is_some()
            || !can_apply(
                &yaml,
                &namespace,
                self.force,
                &prune,
                self.previewed.as_ref(),
            )
        {
            return;
        }
        if let Err(message) = self.core.ensure_writable(&self.cluster) {
            self.notice = Some(Notice::Error(message.into()));
            cx.notify();
            return;
        }
        let Some(doomed) = self
            .previewed
            .as_ref()
            .map(|p| p.plan.prune.iter().map(describe_target).collect::<Vec<_>>())
            .filter(|list| !list.is_empty())
        else {
            self.run_apply(window, cx);
            return;
        };
        // Deleting cannot be undone, so say exactly what goes.
        let count = doomed.len();
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog
                .title(format!("Apply and delete {count} object(s)?"))
                .description(
                    v_flex()
                        .gap_0p5()
                        .child(
                            "These were applied from this manifest before and are no longer in it:",
                        )
                        .children(doomed.iter().map(|line| div().child(line.clone()))),
                )
                .ok_text(format!("Apply and delete {count}"))
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    owner.update(cx, |this, cx| this.run_apply(window, cx)).ok();
                    true
                })
        });
    }

    fn run_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (yaml, namespace, prune) =
            (self.text(cx), self.namespace_text(cx), self.prune_text(cx));
        let Some(allowed) = self.previewed.as_ref().map(|p| p.plan.prune.clone()) else {
            return;
        };
        let force = self.force;
        self.busy = Some("Applying…");
        self.notice = None;
        cx.notify();

        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            let scope = (!namespace.is_empty()).then_some(namespace);
            let request = (!prune.is_empty()).then_some(PruneRequest {
                selector: &prune,
                allowed: &allowed,
            });
            manifest_ops::apply(
                &core,
                &cluster,
                &yaml,
                scope.as_deref(),
                force,
                request.as_ref(),
            )
            .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(results) => {
                        let text = summary(&results);
                        let failed = results
                            .iter()
                            .any(|r| r.status == "error" || r.status == "conflict");
                        this.previewed = None;
                        this.results = Some(results);
                        if failed {
                            toast_error(window, cx, "Some documents were not applied", text);
                        } else {
                            toast_success(window, cx, text);
                        }
                    }
                    Err(reason) => this.notice = Some(Notice::Error(reason.into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_prune(&mut self, cx: &mut Context<Self>) {
        self.prune = !self.prune;
        // A preview was made without, or with, the deletions.
        self.previewed = None;
        cx.notify();
    }

    fn toggle_force(&mut self, cx: &mut Context<Self>) {
        self.force = !self.force;
        // A preview was made under the other setting.
        self.previewed = None;
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };
        let idle = self.busy.is_none();
        let (yaml, namespace, prune) =
            (self.text(cx), self.namespace_text(cx), self.prune_text(cx));
        let applicable = idle
            && can_apply(
                &yaml,
                &namespace,
                self.force,
                &prune,
                self.previewed.as_ref(),
            );
        let contested = self
            .previewed
            .as_ref()
            .is_some_and(|p| p.plan.docs.iter().any(|d| d.action == "conflict"));

        let templates = templates::TEMPLATES
            .iter()
            .enumerate()
            .map(|(index, template)| {
                chip(
                    ("create-template", index),
                    template.label,
                    true,
                    false,
                    cx.theme(),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.use_template(index, window, cx)),
                )
            });

        let open_kind = self.form.as_ref().map(|(form, _)| form.read(cx).kind());
        let forms_row = k8s_app::forms::SPECS
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let kind = spec.kind;
                chip(
                    ("create-form", index),
                    kind,
                    true,
                    open_kind == Some(kind),
                    cx.theme(),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.open_form(kind, window, cx)))
            })
            .collect::<Vec<_>>();

        v_flex()
            .flex_none()
            .px_4()
            .pb_2()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(div().text_xs().text_color(muted).child("Start from"))
                    .children(templates),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(div().text_xs().text_color(muted).child("Or fill in a form"))
                    .children(forms_row),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        button_icon(
                            "create-import",
                            IconName::Upload,
                            "Import file…",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| {
                            b.on_click(
                                cx.listener(|this, _, window, cx| this.choose(false, window, cx)),
                            )
                        }),
                    )
                    .child(
                        button_icon(
                            "create-kustomize",
                            IconName::Hammer,
                            "Build kustomize folder…",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| {
                            b.on_click(
                                cx.listener(|this, _, window, cx| this.choose(true, window, cx)),
                            )
                        }),
                    )
                    .child(div().text_xs().text_color(muted).child("Namespace"))
                    .child(div().w(px(160.)).child(Input::new(&self.namespace)))
                    .child(
                        button_icon(
                            "create-preview",
                            IconName::Eye,
                            "Preview",
                            idle,
                            Emphasis::Plain,
                            cx.theme(),
                        )
                        .when(idle, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.preview(cx)))
                        }),
                    )
                    .child(
                        button_icon(
                            "create-apply",
                            IconName::Check,
                            "Apply",
                            applicable,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(applicable, |b| {
                            b.on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)))
                        }),
                    )
                    .child(
                        chip("create-prune", "Prune", idle, self.prune, cx.theme())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_prune(cx))),
                    )
                    .when(self.prune, |bar| {
                        bar.child(div().w(px(160.)).child(Input::new(&self.prune_selector)))
                    })
                    .when(contested || self.force, |bar| {
                        bar.child(
                            chip(
                                "create-force",
                                "Take ownership",
                                idle,
                                self.force,
                                cx.theme(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_force(cx))),
                        )
                    })
                    .when_some(self.busy, |bar, text| {
                        bar.child(div().text_xs().text_color(muted).child(text))
                    }),
            )
    }

    fn render_plan(&self, previewed: &Previewed, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, danger, warning) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.warning,
            )
        };
        let selected = self
            .selected
            .min(previewed.plan.docs.len().saturating_sub(1));
        let rows = previewed.plan.docs.iter().enumerate().map(|(index, doc)| {
            let color = status_color(&doc.action, cx.theme());
            let (active, hover) = (cx.theme().list_active, cx.theme().secondary);
            h_flex()
                .id(("create-doc", index))
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .text_sm()
                .cursor_pointer()
                .when(index == selected, |row| row.bg(active))
                .hover(|row| row.bg(hover))
                .child(pill(doc.action.clone(), color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{} {}", doc.kind, doc.name)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(doc.namespace.clone().unwrap_or_default()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = index;
                    cx.notify();
                }))
        });

        let prune_rows = previewed
            .plan
            .prune
            .iter()
            .enumerate()
            .map(|(index, target)| {
                h_flex()
                    .id(("create-prune-row", index))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(pill("delete", status_color("delete", cx.theme())))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("{} {}", target.kind, target.name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(target.namespace.clone().unwrap_or_default()),
                    )
            });

        let detail = match previewed.plan.docs.get(selected) {
            Some(doc) if doc.error.is_some() => div()
                .p_3()
                .text_sm()
                .text_color(danger)
                .child(doc.error.clone().unwrap_or_default())
                .into_any_element(),
            Some(doc) => v_flex()
                .flex_1()
                .min_h_0()
                .children(doc.warnings.iter().map(|text| {
                    div()
                        .px_3()
                        .pt_1()
                        .text_xs()
                        .text_color(warning)
                        .child(text.clone())
                }))
                .children(doc.conflicts.iter().take(6).map(|conflict| {
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(danger)
                        .truncate()
                        .child(format!("{} owns {}", conflict.manager, conflict.field))
                }))
                .child(if doc.action == "unchanged" {
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(muted)
                        .child("Nothing would change.")
                        .into_any_element()
                } else {
                    uniform_list(
                        "create-diff",
                        previewed.diffs.get(selected).map_or(0, Vec::len),
                        cx.processor(move |this, range: Range<usize>, _window, cx| {
                            this.previewed
                                .as_ref()
                                .and_then(|p| p.diffs.get(selected))
                                .map(|lines| diff::rows(lines, range, cx))
                                .unwrap_or_default()
                        }),
                    )
                    .track_scroll(&self.diff_scroll)
                    .flex_1()
                    .into_any_element()
                })
                .into_any_element(),
            None => div().into_any_element(),
        };

        h_flex()
            .flex_none()
            .h(px(240.))
            .border_t_1()
            .border_color(border)
            .child(
                v_flex()
                    .id("create-docs")
                    .w(px(320.))
                    .flex_none()
                    .h_full()
                    .overflow_y_scroll()
                    .border_r_1()
                    .border_color(border)
                    .children(rows)
                    .children(prune_rows),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(detail),
            )
    }

    fn render_results(&self, results: &[DocResult], cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, danger) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground, theme.danger)
        };
        v_flex()
            .id("create-results")
            .flex_none()
            .h(px(200.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(border)
            .children(results.iter().map(|result| {
                v_flex()
                    .px_3()
                    .py_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .child(pill(
                                result.status.clone(),
                                status_color(&result.status, cx.theme()),
                            ))
                            .child(format!("{} {}", result.kind, result.name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(result.namespace.clone().unwrap_or_default()),
                            ),
                    )
                    .when_some(result.error.clone(), |column, error| {
                        column.child(div().text_xs().text_color(danger).child(error))
                    })
            }))
    }
}

impl Render for CreateView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, danger, border_color) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger, theme.border)
        };
        let toolbar = self.render_toolbar(cx).into_any_element();
        let panel = if let Some(previewed) = &self.previewed {
            Some(self.render_plan(previewed, cx).into_any_element())
        } else {
            self.results
                .as_ref()
                .map(|results| self.render_results(results, cx).into_any_element())
        };

        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_none()
                    .px_4()
                    .items_center()
                    .child(page_title(IconName::Plus, "Create", cx.theme())),
            )
            .child(toolbar)
            .when_some(self.notice.as_ref(), |column, notice| {
                let (text, color) = match notice {
                    Notice::Info(text) => (text.clone(), muted),
                    Notice::Error(text) => (text.clone(), danger),
                };
                column.child(div().px_4().py_1().text_xs().text_color(color).child(text))
            })
            .when(self.detached && self.form.is_some(), |column| {
                column.child(
                    h_flex()
                        .px_4()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("You edited the YAML, so the form no longer changes it."),
                        )
                        .child(
                            chip(
                                "create-form-again",
                                "Let the form write it again",
                                true,
                                false,
                                cx.theme(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.use_form_again(cx))),
                        ),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when_some(self.form.as_ref(), |row, (form, _)| {
                        row.child(
                            div()
                                .w(px(440.))
                                .h_full()
                                .flex_none()
                                .border_r_1()
                                .border_color(border_color)
                                .child(form.clone()),
                        )
                    })
                    .child(
                        div().flex_1().min_w_0().h_full().child(
                            v_flex()
                                .size_full()
                                .child(div().flex_1().min_h_0().child(
                                    Editor::new(&self.editor).appearance(false).h(relative(1.)),
                                ))
                                .child(self.validation.clone()),
                        ),
                    ),
            )
            .children(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::{Previewed, applicable, can_apply, describe_target, summary};
    use k8s_ops::manifest::{DocPlan, DocResult, ManifestPlan};

    fn doc(action: &str) -> DocPlan {
        DocPlan {
            index: 0,
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            name: "a".into(),
            namespace: None,
            resource: None,
            action: action.into(),
            unified: String::new(),
            conflicts: Vec::new(),
            warnings: Vec::new(),
            error: None,
        }
    }

    fn previewed(yaml: &str, namespace: &str, force: bool, actions: &[&str]) -> Previewed {
        Previewed {
            yaml: yaml.into(),
            namespace: namespace.into(),
            force,
            prune: String::new(),
            plan: ManifestPlan {
                docs: actions.iter().map(|a| doc(a)).collect(),
                prune: Vec::new(),
            },
            diffs: Vec::new(),
        }
    }

    #[test]
    fn a_plan_applies_only_if_something_changes_and_nothing_is_wrong() {
        let plan = |actions: &[&str]| ManifestPlan {
            docs: actions.iter().map(|a| doc(a)).collect(),
            prune: Vec::new(),
        };
        assert!(applicable(&plan(&["create", "unchanged"])));
        assert!(applicable(&plan(&["update"])));
        assert!(!applicable(&plan(&[])), "an empty plan");
        assert!(!applicable(&plan(&["unchanged"])), "nothing would change");
        assert!(!applicable(&plan(&["create", "error"])), "one bad document");
        assert!(
            !applicable(&plan(&["update", "conflict"])),
            "a contested field"
        );
    }

    #[test]
    fn apply_needs_a_preview_of_exactly_this_text_namespace_and_setting() {
        let p = previewed("a: 1", "shop", false, &["create"]);
        assert!(can_apply("a: 1", "shop", false, "", Some(&p)));
        assert!(
            !can_apply("a: 1", "shop", false, "", None),
            "never previewed"
        );
        assert!(
            !can_apply("a: 2", "shop", false, "", Some(&p)),
            "text edited"
        );
        assert!(
            !can_apply("a: 1", "other", false, "", Some(&p)),
            "namespace edited"
        );
        assert!(
            !can_apply("a: 1", "shop", true, "", Some(&p)),
            "ownership changed"
        );
    }

    #[test]
    fn a_plan_that_only_prunes_can_apply_and_a_changed_selector_needs_a_new_preview() {
        let target = k8s_ops::manifest::PruneTarget {
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            name: "old".into(),
            namespace: Some("shop".into()),
            resource: "core/v1/configmaps".into(),
        };
        let only_prune = ManifestPlan {
            docs: vec![doc("unchanged")],
            prune: vec![target.clone()],
        };
        assert!(applicable(&only_prune));
        assert_eq!(describe_target(&target), "ConfigMap shop/old");

        let mut p = previewed("a: 1", "shop", false, &["unchanged"]);
        p.plan = only_prune;
        p.prune = "app=web".into();
        assert!(can_apply("a: 1", "shop", false, "app=web", Some(&p)));
        assert!(!can_apply("a: 1", "shop", false, "app=api", Some(&p)));
        assert!(!can_apply("a: 1", "shop", false, "", Some(&p)));
    }

    #[test]
    fn the_summary_counts_each_outcome_and_skips_empty_ones() {
        let result = |status: &str| DocResult {
            index: 0,
            kind: "ConfigMap".into(),
            name: "a".into(),
            namespace: None,
            status: status.into(),
            conflicts: Vec::new(),
            error: None,
        };
        let all = [
            result("created"),
            result("created"),
            result("error"),
            result("conflict"),
            result("pruned"),
        ];
        assert_eq!(summary(&all), "2 created, 1 pruned, 2 failed");
        assert_eq!(summary(&[result("unchanged")]), "1 unchanged");
        assert_eq!(summary(&[]), "Nothing applied");
    }
}
