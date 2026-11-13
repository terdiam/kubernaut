//! The lines a log view keeps, and which objects have logs at all.
//!
//! Pure data, so the rules are unit-tested without a window.

use std::collections::VecDeque;

use gpui_kit::SharedString;
use k8s_ops::logs::{LogEvent, LogTarget};

/// What a line is, which decides how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Output,
    /// Something about the stream rather than from the container.
    Notice,
    Error,
}

pub struct LogLine {
    /// Set only when several pods share the view, so a single pod's logs are
    /// not cluttered with a prefix that never changes.
    pub pod: Option<SharedString>,
    pub text: SharedString,
    pub kind: LineKind,
}

/// The newest lines, oldest dropped first. A busy pod can write far faster than
/// anyone reads, so the view is bounded rather than growing until memory runs
/// out.
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    capacity: usize,
    /// Each pod's display label, worked out once: a stream repeats a handful
    /// of names millions of times, so the label is not rebuilt per line.
    pods: Vec<(SharedString, SharedString)>,
    multi_pod: bool,
}

impl LogBuffer {
    pub fn new(capacity: usize, multi_pod: bool) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            pods: Vec::new(),
            multi_pod,
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&LogLine> {
        self.lines.get(index)
    }

    pub fn push_all(&mut self, events: &[LogEvent]) {
        for event in events {
            self.push(event);
        }
    }

    fn push(&mut self, event: &LogEvent) {
        let line = match event {
            LogEvent::Line { pod, text, .. } => LogLine {
                pod: self.multi_pod.then(|| self.label_for(pod)),
                text: text.trim_end_matches(['\r', '\n']).into(),
                kind: LineKind::Output,
            },
            LogEvent::Dropped { count } => LogLine {
                pod: None,
                text: format!("{count} lines were dropped because the view fell behind").into(),
                kind: LineKind::Notice,
            },
            LogEvent::PodEnded { pod, reason } => LogLine {
                pod: None,
                text: format!("{pod} ended: {reason}").into(),
                kind: LineKind::Notice,
            },
            LogEvent::PodFailed { pod, message } => LogLine {
                pod: None,
                text: format!("{pod}: {message}").into(),
                kind: LineKind::Error,
            },
        };
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn label_for(&mut self, pod: &str) -> SharedString {
        if let Some((_, label)) = self.pods.iter().find(|(known, _)| **known == *pod) {
            return label.clone();
        }
        let label = tail_label(pod);
        self.pods.push((pod.into(), label.clone()));
        label
    }
}

/// The tail of a pod name, which is the part that tells replicas apart: every
/// pod of a Deployment shares a long prefix and differs only at the end.
fn tail_label(pod: &str) -> SharedString {
    const KEEP: usize = 16;
    // The `KEEP + 1`th character from the end exists only if the name is
    // longer than `KEEP`; what follows it is exactly the last `KEEP`.
    match pod.char_indices().rev().nth(KEEP) {
        Some((at, c)) => format!("…{}", &pod[at + c.len_utf8()..]).into(),
        None => pod.into(),
    }
}

/// Resources whose logs are the logs of the pods behind them.
const WORKLOADS: &[&str] = &[
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/daemonsets",
    "apps/v1/replicasets",
    "batch/v1/jobs",
    "core/v1/replicationcontrollers",
    "core/v1/services",
];

/// What to tail for an object, or `None` when it has no logs of its own.
pub fn log_target(resource: &str, namespace: Option<&str>, name: &str) -> Option<LogTarget> {
    let namespace = namespace?.to_string();
    if resource == "core/v1/pods" {
        Some(LogTarget::Pod {
            namespace,
            name: name.to_string(),
        })
    } else if WORKLOADS.contains(&resource) {
        Some(LogTarget::Workload {
            namespace,
            resource: resource.to_string(),
            name: name.to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{LineKind, LogBuffer, log_target};
    use k8s_ops::logs::{LogEvent, LogTarget};

    fn line(pod: &str, text: &str) -> LogEvent {
        LogEvent::Line {
            pod: pod.into(),
            container: "app".into(),
            text: text.into(),
        }
    }

    #[test]
    fn the_buffer_keeps_only_the_newest_lines() {
        let mut buffer = LogBuffer::new(3, false);
        buffer.push_all(&[
            line("p", "1"),
            line("p", "2"),
            line("p", "3"),
            line("p", "4"),
        ]);
        assert_eq!(buffer.len(), 3);
        assert_eq!(
            &*buffer.get(0).map(|l| l.text.clone()).unwrap_or_default(),
            "2"
        );
        assert_eq!(
            &*buffer.get(2).map(|l| l.text.clone()).unwrap_or_default(),
            "4"
        );
        assert!(buffer.get(3).is_none());
    }

    #[test]
    fn trailing_newlines_are_trimmed() {
        let mut buffer = LogBuffer::new(8, false);
        buffer.push_all(&[line("p", "hello\r\n")]);
        assert_eq!(
            &*buffer.get(0).map(|l| l.text.clone()).unwrap_or_default(),
            "hello"
        );
    }

    #[test]
    fn a_pod_prefix_appears_only_when_several_pods_share_the_view() {
        let mut single = LogBuffer::new(8, false);
        single.push_all(&[line("web-1", "x")]);
        assert!(single.get(0).is_some_and(|l| l.pod.is_none()));

        let mut many = LogBuffer::new(8, true);
        many.push_all(&[line("web-1", "x"), line("web-2", "y"), line("web-1", "z")]);
        assert_eq!(many.get(0).and_then(|l| l.pod.as_deref()), Some("web-1"));
        assert_eq!(many.get(1).and_then(|l| l.pod.as_deref()), Some("web-2"));
    }

    #[test]
    fn long_pod_names_keep_the_tail_that_tells_replicas_apart() {
        let mut buffer = LogBuffer::new(8, true);
        buffer.push_all(&[
            line("logdemo-6bccb59ff4-pfk2m", "a"),
            line("logdemo-6bccb59ff4-tqbwd", "b"),
            line("logdemo-6bccb59ff4-pfk2m", "c"),
        ]);
        let label = |ix: usize| {
            buffer
                .get(ix)
                .and_then(|l| l.pod.as_deref().map(str::to_string))
                .unwrap_or_default()
        };
        assert!(label(0).starts_with('…') && label(0).ends_with("pfk2m"));
        assert!(label(1).ends_with("tqbwd"));
        assert_ne!(label(0), label(1));
        assert_eq!(label(0), label(2), "the same pod keeps the same label");
        assert_eq!(label(0).chars().count(), 17, "an ellipsis and the last 16");
    }

    #[test]
    fn stream_events_become_notices_and_errors() {
        let mut buffer = LogBuffer::new(8, true);
        buffer.push_all(&[
            LogEvent::Dropped { count: 12 },
            LogEvent::PodEnded {
                pod: "web-1".into(),
                reason: "deleted".into(),
            },
            LogEvent::PodFailed {
                pod: "web-2".into(),
                message: "log file not found".into(),
            },
        ]);
        let kinds: Vec<_> = (0..3)
            .filter_map(|ix| buffer.get(ix).map(|l| l.kind))
            .collect();
        assert_eq!(kinds, [LineKind::Notice, LineKind::Notice, LineKind::Error]);
        assert!(
            buffer
                .get(0)
                .is_some_and(|l| l.text.contains("12 lines were dropped"))
        );
    }

    #[test]
    fn pods_log_themselves_and_workloads_log_their_pods() {
        assert!(matches!(
            log_target("core/v1/pods", Some("ns"), "p"),
            Some(LogTarget::Pod { .. })
        ));
        assert!(matches!(
            log_target("apps/v1/deployments", Some("ns"), "d"),
            Some(LogTarget::Workload { .. })
        ));
        assert!(log_target("core/v1/configmaps", Some("ns"), "c").is_none());
        assert!(
            log_target("core/v1/pods", None, "p").is_none(),
            "logs need a namespace"
        );
    }
}
