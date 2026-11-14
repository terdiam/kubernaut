//! The Diagnose tab: why a pod is not running, quoting the cluster.
//!
//! Each finding states what it means, shows the exact text the cluster
//! produced as evidence, and ends in steps that open the right tab, open the
//! object that is actually at fault, or hand over a `kubectl` line to paste.

use std::sync::Arc;

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, present::Tone};
use k8s_ops::diagnose::{self, DiagnosisReport, StepAction};
use tokio::runtime::Handle;

use crate::{
    drawer::Target,
    ui::{Palette, chip_icon, message, pill, toast_success, tone_color},
};

pub enum DiagnoseEvent {
    /// A step asks the drawer to go somewhere.
    Action(StepAction),
}

/// What a step's button says.
fn action_label(action: &StepAction) -> String {
    match action {
        StepAction::Logs { previous: true, .. } => "Previous logs".to_string(),
        StepAction::Logs { .. } => "Logs".to_string(),
        StepAction::Terminal => "Shell".to_string(),
        StepAction::Edit => "Edit YAML".to_string(),
        StepAction::Open { resource, name, .. } => {
            format!("Open {} {name}", kind_of(resource).to_lowercase())
        }
    }
}

/// A readable kind from a `group/version/plural` key, for a label. The plural
/// minus its `s` is right for the objects a diagnosis points at (nodes, pods,
/// volume claims); anything stranger is still a legible name.
pub fn kind_of(resource: &str) -> String {
    let plural = resource.rsplit('/').next().unwrap_or(resource);
    let singular = plural.strip_suffix('s').unwrap_or(plural);
    let mut chars = singular.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn tone_of(severity: &str) -> Tone {
    match severity {
        "error" => Tone::Error,
        "warning" => Tone::Warn,
        _ => Tone::Pending,
    }
}

struct StepLine {
    text: SharedString,
    command: Option<SharedString>,
    action: Option<StepAction>,
}

struct FindingLine {
    tone: Tone,
    title: SharedString,
    explanation: SharedString,
    evidence: Vec<SharedString>,
    steps: Vec<StepLine>,
}

struct PodBlock {
    pod: SharedString,
    phase: SharedString,
    summary: SharedString,
    findings: Vec<FindingLine>,
}

/// What the tab shows, built once from the backend's report.
struct Shown {
    pods: Vec<PodBlock>,
    examined: usize,
    healthy: usize,
    truncated: bool,
}

fn shown(report: DiagnosisReport) -> Shown {
    Shown {
        pods: report
            .pods
            .into_iter()
            .map(|diagnosis| PodBlock {
                pod: diagnosis.pod.into(),
                phase: diagnosis.phase.into(),
                summary: diagnosis.summary.into(),
                findings: diagnosis
                    .findings
                    .into_iter()
                    .map(|finding| FindingLine {
                        tone: tone_of(&finding.severity),
                        title: finding.title.into(),
                        explanation: finding.explanation.into(),
                        evidence: finding.evidence.into_iter().map(Into::into).collect(),
                        steps: finding
                            .steps
                            .into_iter()
                            .map(|step| StepLine {
                                text: step.text.into(),
                                command: step.command.map(Into::into),
                                action: step.action,
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
        examined: report.examined,
        healthy: report.healthy,
        truncated: report.truncated,
    }
}

enum Load {
    Loading,
    Ready(Shown),
    Failed(SharedString),
}

pub struct DiagnoseView {
    core: Arc<AppCore>,
    runtime: Handle,
    target: Target,
    load: Load,
}

impl EventEmitter<DiagnoseEvent> for DiagnoseView {}

impl DiagnoseView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        target: &Target,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            target: target.clone(),
            load: Load::Loading,
        };
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        let (core, target) = (Arc::clone(&self.core), self.target.clone());
        let task = self.runtime.spawn(async move {
            let handle = core
                .clusters
                .require(&target.cluster)
                .map_err(|err| err.to_string())?;
            diagnose::diagnose(
                &handle,
                &target.resource,
                target.namespace.as_deref(),
                &target.name,
            )
            .await
            .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update(cx, |this, cx| {
                this.load = match result {
                    Ok(report) => Load::Ready(shown(report)),
                    Err(reason) => Load::Failed(reason.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_finding(
        &self,
        pod: usize,
        index: usize,
        finding: &FindingLine,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (border, muted, secondary) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground, theme.secondary)
        };
        let color = tone_color(finding.tone, cx.theme());

        let mut steps = v_flex().gap_2();
        for (step_index, step) in finding.steps.iter().enumerate() {
            let id = (pod * 1000 + index) * 100 + step_index;
            let action = step.action.clone().map(|action| {
                let label = action_label(&action);
                chip_icon(
                    ("diagnose-action", id),
                    IconName::ArrowRight,
                    label,
                    true,
                    false,
                    cx.theme(),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(DiagnoseEvent::Action(action.clone()));
                }))
            });
            let command = step.command.clone().map(|command| {
                let copy = command.clone();
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(secondary)
                            .text_xs()
                            .font_family("Menlo")
                            .truncate()
                            .child(command),
                    )
                    .child(
                        chip_icon(
                            ("diagnose-copy", id),
                            IconName::Copy,
                            "Copy",
                            true,
                            false,
                            cx.theme(),
                        )
                        .on_click(cx.listener(move |_, _, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
                            toast_success(window, cx, "Copied the command");
                        })),
                    )
            });
            steps = steps.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .w(px(18.))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{}.", step_index + 1)),
                            )
                            .child(div().flex_1().min_w_0().text_sm().child(step.text.clone()))
                            .children(action),
                    )
                    .children(command),
            );
        }

        v_flex()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                    .child(div().font_semibold().child(finding.title.clone())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(finding.explanation.clone()),
            )
            .when(!finding.evidence.is_empty(), |card| {
                card.child(
                    v_flex().gap_1().p_2().rounded_md().bg(secondary).children(
                        finding
                            .evidence
                            .iter()
                            .map(|line| div().text_xs().font_family("Menlo").child(line.clone())),
                    ),
                )
            })
            .child(steps)
    }
}

impl Render for DiagnoseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let body: AnyElement = match &self.load {
            Load::Loading => message("Looking at the pods…", palette.muted).into_any_element(),
            Load::Failed(reason) => message(reason.clone(), palette.danger).into_any_element(),
            Load::Ready(shown) if shown.pods.is_empty() => message(
                format!(
                    "Nothing wrong found. {} {} healthy.",
                    shown.healthy,
                    if shown.healthy == 1 {
                        "pod is"
                    } else {
                        "pods are"
                    }
                ),
                palette.muted,
            )
            .into_any_element(),
            Load::Ready(shown) => {
                let mut column = v_flex().p_3().gap_4();
                if shown.examined > 1 {
                    column =
                        column.child(div().text_xs().text_color(palette.muted).child(format!(
                            "{} of {} pods need attention{}.",
                            shown.pods.len(),
                            shown.examined,
                            if shown.truncated {
                                "; only the first were examined"
                            } else {
                                ""
                            }
                        )));
                }
                for (pod, block) in shown.pods.iter().enumerate() {
                    let mut card = v_flex().gap_2().child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().font_semibold().child(block.pod.clone()))
                            .child(pill(block.phase.clone(), palette.muted))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted)
                                    .truncate()
                                    .child(block.summary.clone()),
                            ),
                    );
                    for (index, finding) in block.findings.iter().enumerate() {
                        card = card.child(self.render_finding(pod, index, finding, cx));
                    }
                    column = column.child(card);
                }
                div()
                    .id("diagnose-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(column)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .child(
                h_flex().flex_none().px_3().pb_1().child(
                    chip_icon(
                        "diagnose-refresh",
                        IconName::RefreshCw,
                        "Check again",
                        true,
                        false,
                        cx.theme(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{action_label, kind_of, tone_of};
    use k8s_app::present::Tone;
    use k8s_ops::diagnose::StepAction;

    #[test]
    fn a_resource_key_becomes_a_readable_kind() {
        assert_eq!(kind_of("core/v1/nodes"), "Node");
        assert_eq!(
            kind_of("core/v1/persistentvolumeclaims"),
            "Persistentvolumeclaim"
        );
        assert_eq!(kind_of("apps/v1/deployments"), "Deployment");
        assert_eq!(kind_of(""), "");
    }

    #[test]
    fn step_buttons_say_where_they_go() {
        assert_eq!(
            action_label(&StepAction::Logs {
                container: None,
                previous: true
            }),
            "Previous logs"
        );
        assert_eq!(action_label(&StepAction::Terminal), "Shell");
        assert_eq!(action_label(&StepAction::Edit), "Edit YAML");
        assert_eq!(
            action_label(&StepAction::Open {
                resource: "core/v1/nodes".into(),
                namespace: None,
                name: "n1".into()
            }),
            "Open node n1"
        );
    }

    #[test]
    fn errors_are_red_warnings_amber_and_the_rest_neutral() {
        assert_eq!(tone_of("error"), Tone::Error);
        assert_eq!(tone_of("warning"), Tone::Warn);
        assert_eq!(tone_of("info"), Tone::Pending);
    }
}
