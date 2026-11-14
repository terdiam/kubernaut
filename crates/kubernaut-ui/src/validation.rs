//! What the cluster's schema says is wrong with an editor's text: underlines in
//! the editor and a short list below it.
//!
//! It advises and never blocks. The apiserver's dry run in Preview is the
//! authority, and a schema that is stricter than the server would only trap
//! people in an editor.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    component::{
        ActiveTheme, h_flex,
        highlighter::{Diagnostic, DiagnosticSeverity},
        input::{EditorState, InputEvent, Position},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{
    AppCore,
    validate_ops::{self, Problem},
};
use tokio::runtime::Handle;

/// Quiet time after the last keystroke before the text is checked.
const PAUSE: Duration = Duration::from_millis(500);
/// Problems listed; the rest are underlined and counted.
const LISTED: usize = 4;

pub struct ValidationView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    editor: Entity<EditorState>,
    problems: Vec<Problem>,
    /// The text `problems` describe.
    checked: SharedString,
    task: Option<Task<()>>,
    _edit: Subscription,
}

impl ValidationView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        editor: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let edit = cx.subscribe_in(editor, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.check(cx);
            }
        });
        Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            cluster,
            editor: editor.clone(),
            problems: Vec::new(),
            checked: SharedString::default(),
            task: None,
            _edit: edit,
        }
    }

    /// Check the editor's text after a pause. Called by whoever sets the text
    /// from code, which the editor does not report as a change.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value();
        if *text == *self.checked {
            return;
        }
        let (core, cluster, runtime) = (
            Arc::clone(&self.core),
            self.cluster.clone(),
            self.runtime.clone(),
        );
        // Replacing the task drops the one still waiting out its pause.
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAUSE).await;
            let job = {
                let text = text.clone();
                runtime
                    .spawn(async move { validate_ops::validate_yaml(&core, &cluster, &text).await })
            };
            let problems = job.await.unwrap_or_default();
            this.update(cx, |this, cx| this.finish(text, problems, cx))
                .ok();
        }));
    }

    fn finish(&mut self, text: SharedString, problems: Vec<Problem>, cx: &mut Context<Self>) {
        // Typed on while the check ran: it describes other text, and the next
        // pause checks the new one.
        if *self.editor.read(cx).value() != *text {
            return;
        }
        self.editor.update(cx, |state, cx| {
            let rope = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
                set.extend(problems.iter().map(diagnostic));
            }
            cx.notify();
        });
        self.problems = problems;
        self.checked = text;
        cx.notify();
    }

    fn jump(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let position = Position::new(to_u32(line), 0);
        self.editor.update(cx, |state, cx| {
            state.set_cursor_position(position, window, cx)
        });
    }
}

fn diagnostic(problem: &Problem) -> Diagnostic {
    let end = problem.end.max(problem.start + 1);
    Diagnostic::new(
        Position::new(to_u32(problem.line), to_u32(problem.start))
            ..Position::new(to_u32(problem.line), to_u32(end)),
        problem.message.clone(),
    )
    .with_severity(DiagnosticSeverity::Error)
    .with_source("schema")
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The header over the list.
fn headline(count: usize) -> String {
    match count {
        1 => "1 problem with the schema".to_string(),
        n => format!("{n} problems with the schema"),
    }
}

impl Render for ValidationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.problems.is_empty() {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let (danger, muted, border, hover) = (
            theme.danger,
            theme.muted_foreground,
            theme.border,
            theme.list_hover,
        );
        let hidden = self.problems.len().saturating_sub(LISTED);

        v_flex()
            .flex_none()
            .max_h(px(160.))
            .overflow_y_hidden()
            .border_t_1()
            .border_color(border)
            .px_3()
            .py_1p5()
            .gap_0p5()
            .text_xs()
            .child(
                div()
                    .text_color(danger)
                    .child(headline(self.problems.len())),
            )
            .children(
                self.problems
                    .iter()
                    .take(LISTED)
                    .enumerate()
                    .map(|(i, problem)| {
                        let line = problem.line;
                        h_flex()
                            .id(("problem", i))
                            .gap_2()
                            .cursor_pointer()
                            .hover(move |row| row.bg(hover))
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.jump(line, window, cx)),
                            )
                            .child(
                                div()
                                    .w(px(48.))
                                    .flex_none()
                                    .text_color(muted)
                                    .child(format!("line {}", line + 1)),
                            )
                            .child(div().min_w_0().child(problem.message.clone()))
                    }),
            )
            .when(hidden > 0, |column| {
                column.child(
                    div()
                        .text_color(muted)
                        .child(format!("and {hidden} more, underlined in the text")),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{diagnostic, headline};
    use k8s_app::validate_ops::Problem;

    #[test]
    fn the_headline_counts() {
        assert_eq!(headline(1), "1 problem with the schema");
        assert_eq!(headline(3), "3 problems with the schema");
    }

    #[test]
    fn an_empty_span_is_widened_so_it_can_be_underlined() {
        let problem = Problem {
            line: 4,
            start: 7,
            end: 7,
            message: "x".into(),
        };
        let found = diagnostic(&problem);
        assert_eq!(
            (found.range.start.line, found.range.start.character),
            (4, 7)
        );
        assert_eq!(found.range.end.character, 8);
    }
}
