//! Settings: the few preferences that change how the app behaves, saved as soon
//! as they change.
//!
//! Only settings that already do something are listed. Each cluster can be
//! marked protected, so changes from this app are refused outright, and given a
//! default namespace.

use std::sync::Arc;

use gpui_kit::{
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, helm_ops, preferences::Preferences};
use tokio::runtime::Handle;

use crate::ui::{segment, segmented_bar, toast_error};

/// How many lines a log view asks for when it opens.
const TAIL_CHOICES: [i64; 4] = [200, 500, 1000, 5000];

/// A namespace as typed: blank clears it, anything else must be one Kubernetes
/// accepts.
fn namespace_edit(value: &str) -> Result<Option<String>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    helm_ops::check_namespace(value)?;
    Ok(Some(value.to_string()))
}

struct ClusterRow {
    name: SharedString,
    protected: bool,
    /// What is saved, so leaving the field untouched does not write the file.
    saved: Option<String>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    _input: Subscription,
}

pub struct SettingsView {
    core: Arc<AppCore>,
    runtime: Handle,
    tail: i64,
    rows: Vec<ClusterRow>,
    path: Option<SharedString>,
}

impl SettingsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = core.preferences();
        let rows = core
            .clusters
            .contexts()
            .into_iter()
            .enumerate()
            .map(|(index, context)| {
                let saved = preferences.profile(&context.name).default_namespace;
                let input = cx.new(|cx| {
                    let mut state = InputState::new(window, cx).placeholder("All namespaces");
                    if let Some(namespace) = &saved {
                        state.set_value(namespace.clone(), window, cx);
                    }
                    state
                });
                let subscription = cx.subscribe_in(
                    &input,
                    window,
                    move |this, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                            this.commit_namespace(index, window, cx);
                        }
                    },
                );
                ClusterRow {
                    protected: preferences.is_protected(&context.name),
                    name: context.name.into(),
                    saved,
                    input,
                    error: None,
                    _input: subscription,
                }
            })
            .collect();
        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            tail: preferences.log_tail_lines,
            rows,
            path: preferences.path().map(Into::into),
        }
    }

    /// Save one change off the UI thread, and say so if it could not be kept.
    fn persist(
        &self,
        what: &'static str,
        edit: impl FnOnce(&mut Preferences) + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let task = self
            .runtime
            .spawn(async move { core.update_preferences(edit) });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(reason) = task.await.unwrap_or_else(|err| Err(err.to_string())) {
                this.update_in(cx, |_, window, cx| {
                    toast_error(window, cx, format!("Could not save {what}"), reason);
                })
                .ok();
            }
        })
        .detach();
    }

    fn set_tail(&mut self, lines: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.tail = lines;
        self.persist(
            "the log length",
            move |p| p.log_tail_lines = lines,
            window,
            cx,
        );
        cx.notify();
    }

    fn set_protected(
        &mut self,
        index: usize,
        protected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        if row.protected == protected {
            return;
        }
        row.protected = protected;
        let name = row.name.to_string();
        self.persist(
            "the protected list",
            move |p| p.set_protected(&name, protected),
            window,
            cx,
        );
        cx.notify();
    }

    fn commit_namespace(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get_mut(index) else {
            return;
        };
        let typed = row.input.read(cx).value();
        match namespace_edit(&typed) {
            Err(reason) => row.error = Some(reason.into()),
            Ok(namespace) => {
                row.error = None;
                if namespace != row.saved {
                    row.saved = namespace.clone();
                    let name = row.name.to_string();
                    self.persist(
                        "the default namespace",
                        move |p| p.set_default_namespace(&name, namespace.as_deref()),
                        window,
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, danger, secondary) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.secondary,
            )
        };
        let card = || {
            v_flex()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .overflow_hidden()
        };
        let heading = |text: &'static str| {
            div()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(text)
        };
        let line = |title: &'static str, note: &'static str, control: AnyElement| {
            h_flex()
                .px_4()
                .py_3()
                .gap_4()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(title))
                        .child(div().text_xs().text_color(muted).child(note)),
                )
                .child(control)
        };

        let tail = TAIL_CHOICES.iter().enumerate().fold(
            segmented_bar(cx.theme()),
            |bar, (index, lines)| {
                let lines = *lines;
                bar.child(
                    segment(
                        ("settings-tail", index),
                        lines.to_string(),
                        self.tail == lines,
                        cx.theme(),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_tail(lines, window, cx)),
                    ),
                )
            },
        );

        let mut clusters = card();
        let count = self.rows.len();
        for index in 0..count {
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            let (open, locked) = (
                segment(("settings-open", index), "Open", !row.protected, cx.theme()).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.set_protected(index, false, window, cx)
                    }),
                ),
                segment(
                    ("settings-protected", index),
                    "Protected",
                    row.protected,
                    cx.theme(),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_protected(index, true, window, cx)
                })),
            );
            let row = self.rows.get(index);
            let Some(row) = row else { continue };
            clusters = clusters.child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .when(index > 0, |column| column.border_t_1().border_color(border))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_semibold()
                                    .child(row.name.clone()),
                            )
                            .child(segmented_bar(cx.theme()).child(open).child(locked)),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(div().text_sm().text_color(muted).child("Default namespace"))
                            .child(div().w(px(200.)).child(Input::new(&row.input))),
                    )
                    .when_some(row.error.clone(), |column, error| {
                        column.child(div().text_xs().text_color(danger).child(error))
                    }),
            );
        }
        if count == 0 {
            clusters = clusters.child(
                div()
                    .p_4()
                    .text_sm()
                    .text_color(muted)
                    .child("No clusters found in your kubeconfig."),
            );
        }

        div()
            .id("settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(680.))
                    .mx_auto()
                    .p_6()
                    .gap_6()
                    .child(div().text_xl().font_semibold().child("Settings"))
                    .child(v_flex().gap_2().child(heading("LOGS")).child(
                        card().bg(secondary.opacity(0.4)).child(line(
                            "Lines to load",
                            "How much history a log view asks for when it opens.",
                            tail.into_any_element(),
                        )),
                    ))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(heading("CLUSTERS"))
                            .child(div().text_xs().text_color(muted).child(
                                "A protected cluster refuses every change from this app: \
                                         scaling, deleting, applying YAML, Helm, silences. Reading \
                                         still works. The default namespace applies the next time \
                                         the cluster connects.",
                            ))
                            .child(clusters),
                    )
                    .when_some(self.path.clone(), |column, path| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("Saved in {path}")),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::namespace_edit;

    #[test]
    fn a_blank_namespace_clears_and_a_valid_one_is_trimmed() {
        assert_eq!(namespace_edit(""), Ok(None));
        assert_eq!(namespace_edit("   "), Ok(None));
        assert_eq!(namespace_edit(" shop "), Ok(Some("shop".to_string())));
    }

    #[test]
    fn a_namespace_kubernetes_would_refuse_is_rejected_with_a_reason() {
        assert!(namespace_edit("Shop").is_err());
        assert!(namespace_edit("a_b").is_err());
        assert!(namespace_edit("-a").is_err());
    }
}
