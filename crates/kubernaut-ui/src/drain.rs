//! Draining a node: what it will do, asked before it does it, and what it did.
//!
//! The preview comes from a dry run, so the question names the pods and says
//! which are left alone and why. Nothing here knows about a cluster: it reads a
//! report and draws dialogs, with the work handed in as a callback.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    component::{
        ActiveTheme, WindowExt, button::ButtonVariant, checkbox::Checkbox, h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_ops::actions::{DrainReport, SkipKind};

/// Pods listed by name in a dialog; the rest are counted.
const LISTED: usize = 8;

/// A report reduced to the numbers a sentence needs.
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub evict: usize,
    pub daemon_sets: usize,
    pub static_pods: usize,
    /// Pods with no controller, which a drain leaves unless told to delete them.
    pub standalone: usize,
    pub blocked: usize,
}

pub fn summarize(report: &DrainReport) -> Summary {
    let count = |kind: SkipKind| report.skipped.iter().filter(|s| s.kind == kind).count();
    Summary {
        evict: report.evicted.len(),
        daemon_sets: count(SkipKind::DaemonSet),
        static_pods: count(SkipKind::Static),
        standalone: count(SkipKind::Standalone),
        blocked: report.blocked.len(),
    }
}

/// What is left alone, as one clause: "3 DaemonSet pods and 1 static pod".
pub fn left_alone(summary: &Summary) -> Option<String> {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let parts: Vec<String> = [
        (summary.daemon_sets > 0)
            .then(|| plural(summary.daemon_sets, "DaemonSet pod", "DaemonSet pods")),
        (summary.static_pods > 0).then(|| plural(summary.static_pods, "static pod", "static pods")),
        (summary.standalone > 0).then(|| {
            plural(
                summary.standalone,
                "pod with no controller",
                "pods with no controller",
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn names(list: &[String]) -> Vec<String> {
    let mut shown: Vec<String> = list.iter().take(LISTED).cloned().collect();
    if list.len() > LISTED {
        shown.push(format!("and {} more", list.len() - LISTED));
    }
    shown
}

/// Ask before draining. `confirm` gets whether to also evict the pods that have
/// no controller, a choice offered only when there are some.
pub fn open_confirm(
    window: &mut Window,
    cx: &mut App,
    node: &str,
    preview: &DrainReport,
    confirm: impl Fn(bool, &mut Window, &mut App) + Clone + 'static,
) {
    let summary = summarize(preview);
    let evicted = names(&preview.evicted);
    let skipped = left_alone(&summary);
    let node = node.to_string();
    let also_standalone = Rc::new(Cell::new(false));

    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let (confirm, also) = (confirm.clone(), Rc::clone(&also_standalone));
        let toggle = Rc::clone(&also_standalone);
        let evict_now = summary.evict + if also.get() { summary.standalone } else { 0 };

        dialog
            .title(format!("Drain {node}?"))
            .description(
                v_flex()
                    .gap_2()
                    .child(format!(
                        "The node is cordoned, then {evict_now} pod{} evicted through the eviction \
                         API, which respects PodDisruptionBudgets. A pod a budget refuses stays \
                         and is reported.",
                        if evict_now == 1 { " is" } else { "s are" }
                    ))
                    .when(!evicted.is_empty(), |column| {
                        column.child(
                            v_flex().gap_0p5().text_color(muted).children(
                                evicted.iter().map(|name| div().child(name.clone())),
                            ),
                        )
                    })
                    .when_some(skipped.clone(), |column, text| {
                        column.child(div().text_color(muted).child(format!("Left alone: {text}.")))
                    })
                    .when(summary.standalone > 0, |column| {
                        column.child(
                            h_flex().gap_2().items_center().child(
                                Checkbox::new("drain-standalone")
                                    .label(format!(
                                        "Also delete the {} pod(s) with no controller: they will not come back",
                                        summary.standalone
                                    ))
                                    .checked(toggle.get())
                                    .on_click(move |checked, window, _| {
                                        toggle.set(*checked);
                                        window.refresh();
                                    }),
                            ),
                        )
                    }),
            )
            .ok_text("Drain")
            .ok_variant(ButtonVariant::Danger)
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                confirm(also.get(), window, cx);
                true
            })
    });
}

/// Say what the drain did, and what is still on the node and why.
pub fn open_result(window: &mut Window, cx: &mut App, node: &str, report: &DrainReport) {
    let summary = summarize(report);
    let node = node.to_string();
    let incomplete = summary.blocked > 0;
    let evicted = names(&report.evicted);
    let blocked: Vec<(String, String)> = report
        .blocked
        .iter()
        .take(LISTED)
        .map(|pod| (pod.name.clone(), pod.message.clone()))
        .collect();
    let skipped = left_alone(&summary);

    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let (muted, warning) = (cx.theme().muted_foreground, cx.theme().warning);
        dialog
            .title(if incomplete {
                format!("{node} is cordoned, but not empty")
            } else {
                format!("Drained {node}")
            })
            .description(
                v_flex()
                    .gap_2()
                    .child(format!(
                        "{} pod{} evicted; the node is cordoned and takes no new pods.",
                        summary.evict,
                        if summary.evict == 1 { "" } else { "s" }
                    ))
                    .when(!evicted.is_empty(), |column| {
                        column.child(
                            v_flex()
                                .gap_0p5()
                                .text_color(muted)
                                .children(evicted.iter().map(|name| div().child(name.clone()))),
                        )
                    })
                    .when(!blocked.is_empty(), |column| {
                        column.child(
                            v_flex()
                                .gap_0p5()
                                .child(div().text_color(warning).child(format!(
                                    "{} pod{} could not be evicted, usually a PodDisruptionBudget. \
                                     Drain again when it allows it.",
                                    summary.blocked,
                                    if summary.blocked == 1 { "" } else { "s" }
                                )))
                                .children(blocked.iter().map(|(name, message)| {
                                    div().text_color(muted).child(format!("{name}: {message}"))
                                })),
                        )
                    })
                    .when_some(skipped.clone(), |column, text| {
                        column.child(
                            div()
                                .text_color(muted)
                                .child(format!("Left alone: {text}.")),
                        )
                    }),
            )
            .ok_text("Close")
    });
}

#[cfg(test)]
mod tests {
    use k8s_ops::actions::{BlockedPod, DrainReport, SkipKind, SkippedPod};

    use super::{Summary, left_alone, names, summarize};

    fn skipped(kind: SkipKind) -> SkippedPod {
        SkippedPod {
            name: "ns/p".into(),
            reason: String::new(),
            kind,
        }
    }

    fn report() -> DrainReport {
        DrainReport {
            node: "n1".into(),
            evicted: vec!["a/web-1".into(), "a/web-2".into()],
            skipped: vec![
                skipped(SkipKind::DaemonSet),
                skipped(SkipKind::DaemonSet),
                skipped(SkipKind::Static),
                skipped(SkipKind::Standalone),
            ],
            blocked: vec![BlockedPod {
                name: "a/db-0".into(),
                message: "would violate the budget".into(),
            }],
        }
    }

    #[test]
    fn a_report_is_counted_by_why_pods_were_left() {
        assert_eq!(
            summarize(&report()),
            Summary {
                evict: 2,
                daemon_sets: 2,
                static_pods: 1,
                standalone: 1,
                blocked: 1,
            }
        );
    }

    #[test]
    fn what_is_left_alone_reads_as_a_clause_with_the_right_plurals() {
        assert_eq!(
            left_alone(&summarize(&report())).as_deref(),
            Some("2 DaemonSet pods, 1 static pod, 1 pod with no controller")
        );
        let none = DrainReport {
            node: "n".into(),
            evicted: Vec::new(),
            skipped: Vec::new(),
            blocked: Vec::new(),
        };
        assert!(left_alone(&summarize(&none)).is_none());
    }

    #[test]
    fn a_long_list_of_pods_is_cut_and_counted() {
        let many: Vec<String> = (0..12).map(|n| format!("p{n}")).collect();
        let shown = names(&many);
        assert_eq!(shown.len(), 9);
        assert_eq!(shown.last().map(String::as_str), Some("and 4 more"));
        assert_eq!(names(&many[..3]).len(), 3);
    }
}
