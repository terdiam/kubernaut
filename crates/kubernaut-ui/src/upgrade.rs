//! The form behind both Helm upgrade and Helm install: choose the chart and
//! values, preview what would change, then go.
//!
//! Upgrade is a tab of an installed release. Install is the same form opened
//! on a chart, with the release name and namespace to fill in and the chart's
//! own default values to start from.
//!
//! Nothing is upgraded that was not previewed: the button is enabled only for
//! the exact chart, version, values and options a render has just been shown
//! for, so the diff on screen is always the change that will be made.

use std::{ops::Range, sync::Arc};

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    helm_ops::{self, UpgradeRequest},
    present::Tone,
};
use k8s_helm::{DocumentChange, UpgradeDiff, UpgradeOptions};
use tokio::runtime::Handle;

use crate::{
    charts::{self, ChartLine},
    diff::{self, DiffLine},
    ui::{Emphasis, button, chip, toast_success, tone_color},
};

/// Objects listed before the rest are summarised.
const MAX_CHANGES: usize = 30;

pub enum UpgradeEvent {
    Upgraded,
}

/// Everything an upgrade depends on. Two equal values mean the same upgrade.
#[derive(Clone, PartialEq, Eq)]
struct Inputs {
    install: bool,
    namespace: String,
    release: String,
    chart: String,
    version: String,
    values: String,
    atomic: bool,
    wait: bool,
    reset_values: bool,
    create_namespace: bool,
}

impl Inputs {
    fn request(&self) -> UpgradeRequest {
        UpgradeRequest {
            namespace: self.namespace.clone(),
            release: self.release.clone(),
            chart: self.chart.clone(),
            version: (!self.version.is_empty()).then(|| self.version.clone()),
            values: self.values.clone(),
            options: UpgradeOptions {
                atomic: self.atomic,
                wait: self.wait,
                reset_values: self.reset_values,
                create_namespace: self.create_namespace,
                ..UpgradeOptions::default()
            },
        }
    }
}

/// One object an upgrade would add, change or remove.
struct ChangeLine {
    tone: Tone,
    sign: &'static str,
    label: SharedString,
    /// Only regenerated secret material differs.
    generated: bool,
}

fn change_lines(documents: &[DocumentChange]) -> Vec<ChangeLine> {
    documents
        .iter()
        .map(|doc| {
            let (tone, sign) = match doc.change.as_str() {
                "added" => (Tone::Ok, "+"),
                "removed" => (Tone::Error, "−"),
                _ => (Tone::Warn, "~"),
            };
            ChangeLine {
                tone,
                sign,
                label: format!("{} {}", doc.kind, doc.name).into(),
                generated: doc.generated_only,
            }
        })
        .collect()
}

/// A render for one exact set of inputs.
struct Previewed {
    inputs: Inputs,
    changed: bool,
    generated_only: bool,
    changes: Vec<ChangeLine>,
    hidden_changes: usize,
    diff: Vec<DiffLine>,
}

impl Previewed {
    fn new(inputs: Inputs, result: &UpgradeDiff) -> Self {
        let mut changes = change_lines(&result.documents);
        let hidden_changes = changes.len().saturating_sub(MAX_CHANGES);
        changes.truncate(MAX_CHANGES);
        Self {
            inputs,
            changed: result.changed,
            generated_only: result.generated_only,
            changes,
            hidden_changes,
            diff: diff::parse(&result.unified),
        }
    }
}

impl Inputs {
    /// What is wrong with the names an install needs, if anything. An upgrade
    /// has no names to get wrong: they come from the release.
    fn name_problem(&self) -> Option<&'static str> {
        if !self.install {
            return None;
        }
        helm_ops::check_release_name(&self.release)
            .and_then(|()| helm_ops::check_namespace(&self.namespace))
            .err()
    }
}

/// Going ahead is allowed only for inputs a render has been shown for, that
/// name a chart (and, for an install, a valid release and namespace), and that
/// would change something.
fn can_upgrade(current: &Inputs, previewed: Option<&Previewed>) -> bool {
    !current.chart.is_empty()
        && current.name_problem().is_none()
        && previewed.is_some_and(|previewed| previewed.inputs == *current && previewed.changed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Change a release that exists.
    Upgrade,
    /// Create a new release from a chart.
    Install,
}

/// What the form is for. For an install, `release` starts empty and `chart`
/// and `version` are the chart that was picked.
pub struct Subject {
    pub mode: Mode,
    pub cluster: SharedString,
    pub namespace: SharedString,
    pub release: SharedString,
    pub chart: SharedString,
    pub version: SharedString,
}

pub struct UpgradeView {
    core: Arc<AppCore>,
    runtime: Handle,
    mode: Mode,
    cluster: SharedString,
    /// The release's own namespace and name. An install reads them from the
    /// two fields below instead.
    namespace: SharedString,
    release: SharedString,
    release_input: Entity<InputState>,
    namespace_input: Entity<InputState>,
    chart: Entity<InputState>,
    version: Entity<InputState>,
    values: Entity<EditorState>,
    atomic: bool,
    wait: bool,
    reset_values: bool,
    create_namespace: bool,
    loading_defaults: bool,
    previewed: Option<Previewed>,
    busy: Option<&'static str>,
    /// The last failure, kept on screen: a toast would fade before it is read.
    error: Option<SharedString>,
    /// Charts in the configured repositories, for completing the chart field.
    /// Empty when helm or its repositories are not available, which only
    /// means there is nothing to suggest.
    catalog: Vec<ChartLine>,
    diff_scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<UpgradeEvent> for UpgradeView {}

impl UpgradeView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        subject: Subject,
        values: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chart = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder("repo/chart, a local path, or oci://…");
            state.set_value(subject.chart.clone(), window, cx);
            state
        });
        let version = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Version");
            state.set_value(subject.version.clone(), window, cx);
            state
        });
        let release_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("my-release");
            state.set_value(subject.release.clone(), window, cx);
            state
        });
        let namespace_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("namespace");
            state.set_value(subject.namespace.clone(), window, cx);
            state
        });
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("yaml")
                .line_number(true);
            state.set_value(values.to_string(), window, cx);
            state
        });

        // Typing redraws the buttons, which depend on the text.
        let mut subscriptions = Vec::with_capacity(5);
        for input in [&chart, &version, &release_input, &namespace_input] {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ));
        }
        subscriptions.push(
            cx.subscribe_in(&editor, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        );

        let mode = subject.mode;
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            mode,
            cluster: subject.cluster,
            namespace: subject.namespace,
            release: subject.release,
            release_input,
            namespace_input,
            chart,
            version,
            values: editor,
            atomic: true,
            wait: false,
            reset_values: false,
            create_namespace: false,
            loading_defaults: false,
            previewed: None,
            busy: None,
            error: None,
            catalog: Vec::new(),
            diff_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
        .with_catalog(cx);
        // An install starts from the chart's own defaults.
        if mode == Mode::Install {
            view.load_defaults(window, cx);
        }
        view
    }

    fn with_catalog(self, cx: &mut Context<Self>) -> Self {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { helm_ops::search_charts(&core, "").await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(found)) = task.await {
                this.update(cx, |this, cx| {
                    this.catalog = charts::chart_lines(&found);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        self
    }

    /// Fill the chart field from a suggestion. The version is left blank, which
    /// means the newest.
    fn pick_chart(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chart) = self.catalog.get(index) else {
            return;
        };
        let name = chart.name.clone();
        self.chart
            .update(cx, |input, cx| input.set_value(name, window, cx));
    }

    fn inputs(&self, cx: &App) -> Inputs {
        let install = self.mode == Mode::Install;
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        Inputs {
            install,
            namespace: if install {
                text(&self.namespace_input)
            } else {
                self.namespace.to_string()
            },
            release: if install {
                text(&self.release_input)
            } else {
                self.release.to_string()
            },
            chart: self.chart.read(cx).value().trim().to_string(),
            version: self.version.read(cx).value().trim().to_string(),
            values: self.values.read(cx).value().to_string(),
            atomic: self.atomic,
            wait: self.wait,
            reset_values: self.reset_values,
            create_namespace: self.create_namespace,
        }
    }

    /// Replace the values with the chart's defaults, for the chart and version
    /// as typed.
    fn load_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chart = self.chart.read(cx).value().trim().to_string();
        let version = self.version.read(cx).value().trim().to_string();
        if chart.is_empty() || self.loading_defaults {
            return;
        }
        self.loading_defaults = true;
        self.error = None;
        cx.notify();

        let core = Arc::clone(&self.core);
        let task = self.runtime.spawn(async move {
            let version = (!version.is_empty()).then_some(version);
            helm_ops::chart_values(&core, &chart, version.as_deref()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.loading_defaults = false;
                match result {
                    Ok(values) => this
                        .values
                        .update(cx, |editor, cx| editor.set_value(values, window, cx)),
                    Err(message) => this.error = Some(message.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        if self.busy.is_some() || inputs.chart.is_empty() || inputs.name_problem().is_some() {
            return;
        }
        self.busy = Some("Rendering the chart…");
        self.error = None;
        cx.notify();

        let request = inputs.request();
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let install = self.mode == Mode::Install;
        let task = self.runtime.spawn(async move {
            if install {
                helm_ops::preview_install(&core, &cluster, &request).await
            } else {
                helm_ops::preview_upgrade(&core, &cluster, &request).await
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_preview(inputs, result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_preview(
        &mut self,
        inputs: Inputs,
        result: Result<UpgradeDiff, String>,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            // The form changed while helm was rendering: this describes other
            // inputs, so it is dropped rather than shown against them.
            Ok(_) if self.inputs(cx) != inputs => {
                self.error = Some("The form changed during the preview. Preview again.".into());
            }
            Ok(diff) => self.previewed = Some(Previewed::new(inputs, &diff)),
            Err(message) => {
                self.previewed = None;
                self.error = Some(message.into());
            }
        }
        cx.notify();
    }

    fn upgrade(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        if self.busy.is_some() || !can_upgrade(&inputs, self.previewed.as_ref()) {
            return;
        }
        let install = self.mode == Mode::Install;
        self.busy = Some(if install {
            "Installing. This can take a few minutes…"
        } else {
            "Upgrading. This can take a few minutes…"
        });
        self.error = None;
        cx.notify();

        let request = inputs.request();
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self.runtime.spawn(async move {
            if install {
                helm_ops::install(&core, &cluster, &request).await
            } else {
                helm_ops::upgrade(&core, &cluster, &request).await
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                this.finish_upgrade(result, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_upgrade(
        &mut self,
        result: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = None;
        match result {
            Ok(_) => {
                // An upgrade is announced by the release pane it lives in; an
                // install has no pane to do it, so it says so here.
                if self.mode == Mode::Install {
                    let inputs = self.inputs(cx);
                    toast_success(
                        window,
                        cx,
                        format!("Installed {} in {}", inputs.release, inputs.namespace),
                    );
                }
                self.previewed = None;
                cx.emit(UpgradeEvent::Upgraded);
            }
            Err(message) => self.error = Some(message.into()),
        }
        cx.notify();
    }

    fn toggle(&mut self, option: fn(&mut Self) -> &mut bool, cx: &mut Context<Self>) {
        let flag = option(self);
        *flag = !*flag;
        cx.notify();
    }

    fn render_changes(&self, previewed: &Previewed, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);

        let summary: SharedString = if !previewed.changed {
            "Nothing would change.".into()
        } else if previewed.generated_only {
            "Only generated secrets differ, so this is probably not a real change.".into()
        } else {
            let total = previewed.changes.len() + previewed.hidden_changes;
            format!(
                "{total} object{} would change",
                if total == 1 { "" } else { "s" }
            )
            .into()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(summary),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_3()
                    .pb_1p5()
                    .gap_1p5()
                    .flex_wrap()
                    .children(previewed.changes.iter().map(|change| {
                        let color = tone_color(change.tone, cx.theme());
                        h_flex()
                            .gap_1()
                            .px_1p5()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .bg(color.opacity(0.14))
                            .text_color(color)
                            .child(change.sign)
                            .child(change.label.clone())
                            .when(change.generated, |tag| tag.child("(generated)"))
                    }))
                    .when(previewed.hidden_changes > 0, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("and {} more", previewed.hidden_changes)),
                        )
                    }),
            )
            .when(!previewed.diff.is_empty(), |panel| {
                panel.child(
                    uniform_list(
                        "upgrade-diff",
                        previewed.diff.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            this.previewed
                                .as_ref()
                                .map(|previewed| diff::rows(&previewed.diff, range, cx))
                                .unwrap_or_default()
                        }),
                    )
                    .track_scroll(&self.diff_scroll)
                    .flex_1(),
                )
            })
    }
}

impl Render for UpgradeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, danger, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger, theme.border)
        };
        let inputs = self.inputs(cx);
        let idle = self.busy.is_none();
        let can_preview = idle && !inputs.chart.is_empty() && inputs.name_problem().is_none();
        let can_go = idle && can_upgrade(&inputs, self.previewed.as_ref());

        let preview = cx.listener(|this, _, _, cx| this.preview(cx));
        let upgrade = cx.listener(|this, _, window, cx| this.upgrade(window, cx));
        let toggle_atomic = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.atomic, cx));
        let toggle_wait = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.wait, cx));
        let toggle_reset = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.reset_values, cx));
        let toggle_create =
            cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.create_namespace, cx));
        let defaults = cx.listener(|this, _, window, cx| this.load_defaults(window, cx));
        let install = self.mode == Mode::Install;
        let go_label = if install { "Install" } else { "Upgrade" };
        let problem = inputs.name_problem().filter(|_| !inputs.release.is_empty());

        let typed = self.chart.read(cx).value();
        let suggestions = charts::suggest(&self.catalog, &typed, 5);
        let hover = cx.theme().secondary_hover;

        let label = |text: &'static str| {
            div()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(text)
        };

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .when(install, |column| {
                        column
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_end()
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(label("Release name"))
                                            .child(Input::new(&self.release_input)),
                                    )
                                    .child(
                                        v_flex()
                                            .w(px(170.))
                                            .gap_1()
                                            .child(label("Namespace"))
                                            .child(Input::new(&self.namespace_input)),
                                    ),
                            )
                            .when_some(problem, |column, text| {
                                column.child(div().text_xs().text_color(danger).child(text))
                            })
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .items_end()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(label("Chart"))
                                    .child(Input::new(&self.chart)),
                            )
                            .child(
                                v_flex()
                                    .w(px(120.))
                                    .gap_1()
                                    .child(label("Version"))
                                    .child(Input::new(&self.version)),
                            ),
                    )
                    .when(!suggestions.is_empty(), |column| {
                        column.child(
                            v_flex()
                                .rounded_md()
                                .border_1()
                                .border_color(border)
                                .overflow_hidden()
                                .children(suggestions.iter().map(|&index| {
                                    let chart = &self.catalog[index];
                                    h_flex()
                                        .id(("suggestion", index))
                                        .px_2()
                                        .py_1()
                                        .gap_2()
                                        .items_center()
                                        .cursor_pointer()
                                        .text_sm()
                                        .hover(|row| row.bg(hover))
                                        .child(chart.name.clone())
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(chart.version.clone()),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.pick_chart(index, window, cx);
                                        }))
                                })),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                chip(
                                    "atomic",
                                    "Roll back on failure",
                                    true,
                                    self.atomic,
                                    cx.theme(),
                                )
                                .on_click(toggle_atomic),
                            )
                            .child(
                                chip("wait", "Wait until ready", true, self.wait, cx.theme())
                                    .on_click(toggle_wait),
                            )
                            .when(!install, |row| {
                                row.child(
                                    chip(
                                        "reset",
                                        "Reset values",
                                        true,
                                        self.reset_values,
                                        cx.theme(),
                                    )
                                    .on_click(toggle_reset),
                                )
                            })
                            .when(install, |row| {
                                row.child(
                                    chip(
                                        "create-namespace",
                                        "Create namespace",
                                        true,
                                        self.create_namespace,
                                        cx.theme(),
                                    )
                                    .on_click(toggle_create),
                                )
                            }),
                    )
                    .child(h_flex().gap_2().items_center().child(label("Values")).when(
                        install,
                        |row| {
                            row.child(
                                chip(
                                    "defaults",
                                    if self.loading_defaults {
                                        "Loading…"
                                    } else {
                                        "Load chart defaults"
                                    },
                                    !self.loading_defaults,
                                    false,
                                    cx.theme(),
                                )
                                .when(!self.loading_defaults, |chip| chip.on_click(defaults)),
                            )
                        },
                    ))
                    .child(
                        div()
                            .h(px(180.))
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .overflow_hidden()
                            .child(Editor::new(&self.values).appearance(false).h(relative(1.))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                button(
                                    "preview",
                                    "Preview",
                                    can_preview,
                                    Emphasis::Plain,
                                    cx.theme(),
                                )
                                .when(can_preview, |b| b.on_click(preview)),
                            )
                            .child(
                                button("upgrade", go_label, can_go, Emphasis::Primary, cx.theme())
                                    .when(can_go, |b| b.on_click(upgrade)),
                            )
                            .when_some(self.busy, |row, text| {
                                row.child(div().text_xs().text_color(muted).child(text))
                            }),
                    )
                    .when_some(self.error.clone(), |column, text| {
                        column.child(div().text_xs().text_color(danger).child(text))
                    }),
            )
            .when_some(self.previewed.as_ref(), |column, previewed| {
                column.child(self.render_changes(previewed, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Inputs, Previewed, can_upgrade, change_lines};
    use k8s_app::present::Tone;
    use k8s_helm::{DocumentChange, UpgradeDiff};

    fn inputs(chart: &str) -> Inputs {
        Inputs {
            install: false,
            namespace: "prod".into(),
            release: "web".into(),
            chart: chart.into(),
            version: String::new(),
            values: "replicaCount: 2\n".into(),
            atomic: true,
            wait: false,
            reset_values: false,
            create_namespace: false,
        }
    }

    fn rendered(inputs: Inputs, changed: bool) -> Previewed {
        Previewed::new(
            inputs,
            &UpgradeDiff {
                unified: String::new(),
                changed,
                documents: Vec::new(),
                generated_only: false,
            },
        )
    }

    #[test]
    fn upgrade_needs_a_preview_of_exactly_these_inputs_that_changes_something() {
        let current = inputs("repo/web");
        assert!(!can_upgrade(&current, None), "never previewed");
        assert!(can_upgrade(
            &current,
            Some(&rendered(current.clone(), true))
        ));

        let mut edited = current.clone();
        edited.values.push_str("image: v2\n");
        assert!(
            !can_upgrade(&edited, Some(&rendered(current.clone(), true))),
            "edited since the preview"
        );

        let mut other_option = current.clone();
        other_option.reset_values = true;
        assert!(
            !can_upgrade(&other_option, Some(&rendered(current.clone(), true))),
            "an option changes the result, so it needs its own preview"
        );

        assert!(
            !can_upgrade(&current, Some(&rendered(current.clone(), false))),
            "nothing would change"
        );
    }

    #[test]
    fn an_install_needs_a_valid_release_name_and_namespace_but_an_upgrade_does_not() {
        let mut install = inputs("repo/web");
        install.install = true;
        assert!(can_upgrade(
            &install,
            Some(&rendered(install.clone(), true))
        ));

        install.release = "Bad_Name".into();
        assert!(install.name_problem().is_some());
        assert!(
            !can_upgrade(&install, Some(&rendered(install.clone(), true))),
            "a bad name blocks the install even after a preview"
        );

        install.release = "web".into();
        install.namespace = "Not A Namespace".into();
        assert!(install.name_problem().is_some());

        // An upgrade takes both from the release, so they are never checked.
        let mut upgrade = inputs("repo/web");
        upgrade.release = "Bad_Name".into();
        assert!(upgrade.name_problem().is_none());
    }

    #[test]
    fn creating_the_namespace_is_part_of_what_was_previewed() {
        let current = inputs("repo/web");
        let mut other = current.clone();
        other.create_namespace = true;
        assert!(!can_upgrade(&other, Some(&rendered(current, true))));
    }

    #[test]
    fn a_chart_is_required() {
        let blank = inputs("");
        assert!(!can_upgrade(&blank, Some(&rendered(blank.clone(), true))));
    }

    #[test]
    fn a_blank_version_means_the_latest_and_options_reach_the_request() {
        let mut current = inputs("repo/web");
        let request = current.request();
        assert_eq!(request.version, None);
        assert_eq!(
            (request.namespace.as_str(), request.release.as_str()),
            ("prod", "web")
        );
        assert!(request.options.atomic && !request.options.wait);

        current.version = "1.2.3".into();
        current.wait = true;
        let request = current.request();
        assert_eq!(request.version.as_deref(), Some("1.2.3"));
        assert!(request.options.wait);
    }

    #[test]
    fn changes_are_signed_and_toned_by_what_happens_to_the_object() {
        let documents = vec![
            DocumentChange {
                kind: "Deployment".into(),
                name: "web".into(),
                change: "modified".into(),
                generated_only: false,
            },
            DocumentChange {
                kind: "Service".into(),
                name: "web".into(),
                change: "added".into(),
                generated_only: false,
            },
            DocumentChange {
                kind: "Secret".into(),
                name: "tls".into(),
                change: "removed".into(),
                generated_only: true,
            },
        ];
        let lines = change_lines(&documents);
        let signs: Vec<_> = lines.iter().map(|l| l.sign).collect();
        assert_eq!(signs, ["~", "+", "−"]);
        assert_eq!(lines[0].tone, Tone::Warn);
        assert_eq!(lines[1].tone, Tone::Ok);
        assert_eq!(lines[2].tone, Tone::Error);
        assert_eq!(&*lines[0].label, "Deployment web");
        assert!(lines[2].generated && !lines[0].generated);
    }
}
