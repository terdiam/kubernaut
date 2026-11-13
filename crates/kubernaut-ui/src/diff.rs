//! A unified diff, classified for colouring and drawn as virtual rows. Shared
//! by every view that shows what a change would do before it is made.

use std::ops::Range;

use gpui_kit::{component::ActiveTheme, prelude::FluentBuilder, *};

/// Height of one diff row. Fixed, so the list can be virtualised.
pub const ROW_HEIGHT: Pixels = px(18.);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

pub struct DiffLine {
    pub kind: DiffKind,
    pub text: SharedString,
}

/// Classify the lines of a unified diff for colouring.
pub fn parse(unified: &str) -> Vec<DiffLine> {
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

/// The rows of `lines` in `range`, for a `uniform_list` processor.
pub fn rows(lines: &[DiffLine], range: Range<usize>, cx: &App) -> Vec<Stateful<Div>> {
    let theme = cx.theme();
    let (success, danger, muted, info) = (
        theme.success,
        theme.danger,
        theme.muted_foreground,
        theme.info,
    );
    let mono = theme.mono_font_family.clone();

    lines
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
                .h(ROW_HEIGHT)
                .px_3()
                .text_xs()
                .font_family(mono.clone())
                .truncate()
                .when_some(color, |row, color| row.text_color(color))
                .child(line.text.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{DiffKind, parse};

    #[test]
    fn unified_diff_lines_are_classified_for_colouring() {
        let diff = "--- live\n+++ proposed\n@@ -1,3 +1,3 @@\n a: 1\n-b: 2\n+b: 3\n";
        let kinds: Vec<_> = parse(diff).iter().map(|l| l.kind).collect();
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
        assert!(parse("").is_empty());
    }
}
