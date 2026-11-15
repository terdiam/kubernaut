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
    /// Sequence number the next line gets. A line's number never changes while
    /// its index shifts as old lines drop, so matches are kept by number.
    next_seq: u64,
    /// Lowercase text a line must contain to be shown. Empty shows everything.
    needle: String,
    /// Sequence numbers of the matching lines, oldest first, kept up to date
    /// as lines arrive so a filtered view never rescans the buffer per frame.
    matches: VecDeque<u64>,
}

/// Whether `text` contains `needle`, ignoring case. `needle` is lowercase and
/// not empty. The common ASCII case compares bytes without allocating, since
/// this runs once per line of a busy stream.
fn contains_ci(text: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        return text
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    text.to_lowercase().contains(needle)
}

impl LogBuffer {
    pub fn new(capacity: usize, multi_pod: bool) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            pods: Vec::new(),
            multi_pod,
            next_seq: 0,
            needle: String::new(),
            matches: VecDeque::new(),
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.matches.clear();
    }

    /// Show only lines containing `text`, whatever its case. Blank shows all.
    pub fn set_filter(&mut self, text: &str) {
        self.needle = text.trim().to_lowercase();
        self.matches.clear();
        if self.needle.is_empty() {
            return;
        }
        let first = self.first_seq();
        let needle = &self.needle;
        self.matches.extend(
            self.lines
                .iter()
                .zip(first..)
                .filter(|(line, _)| contains_ci(&line.text, needle))
                .map(|(_, seq)| seq),
        );
    }

    pub fn is_filtering(&self) -> bool {
        !self.needle.is_empty()
    }

    fn first_seq(&self) -> u64 {
        self.next_seq - self.lines.len() as u64
    }

    /// How many lines the view shows: all of them, or those that match.
    pub fn visible_len(&self) -> usize {
        if self.is_filtering() {
            self.matches.len()
        } else {
            self.lines.len()
        }
    }

    /// The `index`th shown line.
    pub fn visible(&self, index: usize) -> Option<&LogLine> {
        if !self.is_filtering() {
            return self.lines.get(index);
        }
        let seq = *self.matches.get(index)?;
        self.lines
            .get(usize::try_from(seq - self.first_seq()).ok()?)
    }

    /// The shown lines as text, one per line, for saving.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for index in 0..self.visible_len() {
            if let Some(line) = self.visible(index) {
                if let Some(pod) = &line.pod {
                    out.push_str(pod);
                    out.push(' ');
                }
                out.push_str(&line.text);
                out.push('\n');
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
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
            let dropped = self.first_seq();
            self.lines.pop_front();
            if self.matches.front() == Some(&dropped) {
                self.matches.pop_front();
            }
        }
        if self.is_filtering() && contains_ci(&line.text, &self.needle) {
            self.matches.push_back(self.next_seq);
        }
        self.next_seq += 1;
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
    use super::{LineKind, LogBuffer, contains_ci, log_target};
    use k8s_ops::logs::{LogEvent, LogTarget};

    fn line(pod: &str, text: &str) -> LogEvent {
        LogEvent::Line {
            pod: pod.into(),
            container: "app".into(),
            text: text.into(),
        }
    }

    fn shown(buffer: &LogBuffer) -> Vec<String> {
        (0..buffer.visible_len())
            .filter_map(|i| buffer.visible(i))
            .map(|l| l.text.to_string())
            .collect()
    }

    #[test]
    fn a_filter_shows_matching_lines_whatever_their_case() {
        let mut buffer = LogBuffer::new(10, false);
        buffer.push_all(&[
            line("p", "GET /a"),
            line("p", "error: boom"),
            line("p", "Error again"),
        ]);
        buffer.set_filter("ERROR");
        assert_eq!(shown(&buffer), ["error: boom", "Error again"]);
        buffer.set_filter("  ");
        assert_eq!(buffer.visible_len(), 3, "blank shows everything");
    }

    #[test]
    fn lines_arriving_after_the_filter_are_matched_as_they_come() {
        let mut buffer = LogBuffer::new(10, false);
        buffer.set_filter("warn");
        buffer.push_all(&[line("p", "ok"), line("p", "WARN x"), line("p", "fine")]);
        assert_eq!(shown(&buffer), ["WARN x"]);
        assert_eq!(buffer.len(), 3, "the rest are kept, only hidden");
    }

    #[test]
    fn matches_survive_the_oldest_lines_dropping() {
        let mut buffer = LogBuffer::new(3, false);
        buffer.set_filter("x");
        buffer.push_all(&[
            line("p", "x1"),
            line("p", "y"),
            line("p", "x2"),
            line("p", "z"),
        ]);
        // `x1` fell off the front: it must not be shown, and `x2` must not move.
        assert_eq!(shown(&buffer), ["x2"]);
        buffer.push_all(&[line("p", "x3"), line("p", "q")]);
        assert_eq!(shown(&buffer), ["x3"]);
        buffer.set_filter("");
        assert_eq!(buffer.visible_len(), 3);
    }

    #[test]
    fn saving_writes_the_shown_lines_with_the_pod_when_several_share_the_view() {
        let mut buffer = LogBuffer::new(10, true);
        buffer.push_all(&[line("web-1", "hello"), line("web-2", "bye")]);
        buffer.set_filter("hello");
        assert_eq!(buffer.to_text(), "web-1 hello\n");
    }

    #[test]
    fn case_folding_handles_non_ascii_text_too() {
        assert!(contains_ci("Ünïcode line", "ünï"));
        assert!(contains_ci("abc", "b"));
        assert!(!contains_ci("abc", "abcd"));
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
            &*buffer
                .visible(0)
                .map(|l| l.text.clone())
                .unwrap_or_default(),
            "2"
        );
        assert_eq!(
            &*buffer
                .visible(2)
                .map(|l| l.text.clone())
                .unwrap_or_default(),
            "4"
        );
        assert!(buffer.visible(3).is_none());
    }

    #[test]
    fn trailing_newlines_are_trimmed() {
        let mut buffer = LogBuffer::new(8, false);
        buffer.push_all(&[line("p", "hello\r\n")]);
        assert_eq!(
            &*buffer
                .visible(0)
                .map(|l| l.text.clone())
                .unwrap_or_default(),
            "hello"
        );
    }

    #[test]
    fn a_pod_prefix_appears_only_when_several_pods_share_the_view() {
        let mut single = LogBuffer::new(8, false);
        single.push_all(&[line("web-1", "x")]);
        assert!(single.visible(0).is_some_and(|l| l.pod.is_none()));

        let mut many = LogBuffer::new(8, true);
        many.push_all(&[line("web-1", "x"), line("web-2", "y"), line("web-1", "z")]);
        assert_eq!(
            many.visible(0).and_then(|l| l.pod.as_deref()),
            Some("web-1")
        );
        assert_eq!(
            many.visible(1).and_then(|l| l.pod.as_deref()),
            Some("web-2")
        );
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
                .visible(ix)
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
            .filter_map(|ix| buffer.visible(ix).map(|l| l.kind))
            .collect();
        assert_eq!(kinds, [LineKind::Notice, LineKind::Notice, LineKind::Error]);
        assert!(
            buffer
                .visible(0)
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
