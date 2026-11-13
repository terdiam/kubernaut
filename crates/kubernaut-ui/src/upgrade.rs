//! The Upgrade tab of a Helm release: choose the chart and values, preview what
//! would change, then upgrade.
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
    diff::{self, DiffLine},
    ui::{Emphasis, button, chip, tone_color},
};

/// Objects listed before the rest are summarised.
const MAX_CHANGES: usize = 30;

pub enum UpgradeEvent {
    Upgraded,
}

/// Everything an upgrade depends on. Two equal values mean the same upgrade.
#[derive(Clone, PartialEq, Eq)]
struct Inputs {
    chart: String,
    version: String,
    values: String,
    atomic: bool,
    wait: bool,
    reset_values: bool,
}

impl Inputs {
    fn request(&self, namespace: &str, release: &str) -> UpgradeRequest {
        UpgradeRequest {
            namespace: namespace.to_string(),
            release: release.to_string(),
            chart: self.chart.clone(),
            version: (!self.version.is_empty()).then(|| self.version.clone()),
            values: self.values.clone(),
            options: UpgradeOptions {
                atomic: self.atomic,
                wait: self.wait,
                reset_values: self.reset_values,
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

/// Upgrade is allowed only for inputs a render has been shown for, that name a
/// chart, and that would change something.
fn can_upgrade(current: &Inputs, previewed: Option<&Previewed>) -> bool {
    !current.chart.is_empty()
        && previewed.is_some_and(|previewed| previewed.inputs == *current && previewed.changed)
}

/// Which release the form is for.
pub struct Subject {
    pub cluster: SharedString,
    pub namespace: SharedString,
    pub release: SharedString,
}

pub struct UpgradeView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    namespace: SharedString,
    release: SharedString,
    chart: Entity<InputState>,
    version: Entity<InputState>,
    values: Entity<EditorState>,
    atomic: bool,
    wait: bool,
    reset_values: bool,
    previewed: Option<Previewed>,
    busy: Option<&'static str>,
    /// The last failure, kept on screen: a toast would fade before it is read.
    error: Option<SharedString>,
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
            InputState::new(window, cx).placeholder("repo/chart, a local path, or oci://…")
        });
        let version = cx.new(|cx| InputState::new(window, cx).placeholder("Version"));
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("yaml")
                .line_number(true);
            state.set_value(values.to_string(), window, cx);
            state
        });

        // Typing redraws the buttons, which depend on the text.
        let mut subscriptions = Vec::with_capacity(3);
        for input in [&chart, &version] {
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

        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster: subject.cluster,
            namespace: subject.namespace,
            release: subject.release,
            chart,
            version,
            values: editor,
            atomic: true,
            wait: false,
            reset_values: false,
            previewed: None,
            busy: None,
            error: None,
            diff_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn inputs(&self, cx: &App) -> Inputs {
        Inputs {
            chart: self.chart.read(cx).value().trim().to_string(),
            version: self.version.read(cx).value().trim().to_string(),
            values: self.values.read(cx).value().to_string(),
            atomic: self.atomic,
            wait: self.wait,
            reset_values: self.reset_values,
        }
    }

    fn preview(&mut self, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        if self.busy.is_some() || inputs.chart.is_empty() {
            return;
        }
        self.busy = Some("Rendering the chart…");
        self.error = None;
        cx.notify();

        let request = inputs.request(&self.namespace, &self.release);
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self
            .runtime
            .spawn(async move { helm_ops::preview_upgrade(&core, &cluster, &request).await });
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

    fn upgrade(&mut self, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        if self.busy.is_some() || !can_upgrade(&inputs, self.previewed.as_ref()) {
            return;
        }
        self.busy = Some("Upgrading. This can take a few minutes…");
        self.error = None;
        cx.notify();

        let request = inputs.request(&self.namespace, &self.release);
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self
            .runtime
            .spawn(async move { helm_ops::upgrade(&core, &cluster, &request).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| this.finish_upgrade(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_upgrade(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        self.busy = None;
        match result {
            Ok(_) => {
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
        let can_preview = idle && !inputs.chart.is_empty();
        let can_go = idle && can_upgrade(&inputs, self.previewed.as_ref());

        let preview = cx.listener(|this, _, _, cx| this.preview(cx));
        let upgrade = cx.listener(|this, _, _, cx| this.upgrade(cx));
        let toggle_atomic = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.atomic, cx));
        let toggle_wait = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.wait, cx));
        let toggle_reset = cx.listener(|this, _, _, cx| this.toggle(|s| &mut s.reset_values, cx));

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
                            .child(
                                chip("reset", "Reset values", true, self.reset_values, cx.theme())
                                    .on_click(toggle_reset),
                            ),
                    )
                    .child(label("Values"))
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
                                button("upgrade", "Upgrade", can_go, Emphasis::Primary, cx.theme())
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
            chart: chart.into(),
            version: String::new(),
            values: "replicaCount: 2\n".into(),
            atomic: true,
            wait: false,
            reset_values: false,
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
    fn a_chart_is_required() {
        let blank = inputs("");
        assert!(!can_upgrade(&blank, Some(&rendered(blank.clone(), true))));
    }

    #[test]
    fn a_blank_version_means_the_latest_and_options_reach_the_request() {
        let mut current = inputs("repo/web");
        let request = current.request("prod", "web");
        assert_eq!(request.version, None);
        assert_eq!(
            (request.namespace.as_str(), request.release.as_str()),
            ("prod", "web")
        );
        assert!(request.options.atomic && !request.options.wait);

        current.version = "1.2.3".into();
        current.wait = true;
        let request = current.request("prod", "web");
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
