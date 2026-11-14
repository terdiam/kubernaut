//! Where in a YAML text a path lives, so a problem can be underlined.
//!
//! `serde_yaml_ng` drops positions when it parses into a `Value`, and a real
//! YAML parser with spans would be a dependency for one feature. Manifests are
//! block-style almost without exception, so this reads indentation: enough to
//! find `spec.containers[1].image`, and when a line is written in flow style
//! (`{a: 1}`) or otherwise not understood, it falls back to the nearest
//! ancestor it did find rather than guessing.

use crate::validate::{Anchor, Path, Segment};

/// A span on one line, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
struct Entry {
    path: Path,
    key: Span,
    value: Option<Span>,
}

/// Every key and list item of one document, with where it was written.
#[derive(Debug, Default)]
pub struct Outline {
    entries: Vec<Entry>,
}

impl Outline {
    /// Index `text`. `first_line` is the line of its first row in the file it
    /// was cut from, so spans stay in file coordinates.
    pub fn build(text: &str, first_line: usize) -> Self {
        let mut outline = Self::default();
        // Open parents, each with the column it was written at.
        let mut stack: Vec<(usize, Segment)> = Vec::new();
        let mut skip_deeper_than: Option<usize> = None;
        let mut flow_depth: i32 = 0;

        for (offset, raw) in text.lines().enumerate() {
            let line = first_line + offset;
            let trimmed = raw.trim_start();
            if flow_depth > 0 {
                flow_depth += bracket_balance(trimmed);
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = raw.len() - trimmed.len();
            if let Some(limit) = skip_deeper_than {
                if indent > limit {
                    continue;
                }
                skip_deeper_than = None;
            }

            let mut column = indent;
            let mut rest = trimmed;
            // `- - a` and `- key: v` both open items before the content.
            while let Some(after) = strip_dash(rest) {
                let gap = rest.len() - after.len();
                close_for_item(&mut stack, column);
                let index = next_index(&outline, &stack);
                stack.push((column, Segment::Index(index)));
                let path = path_of(&stack);
                let span = Span {
                    line,
                    start: chars_before(raw, column),
                    end: chars_before(raw, column) + 1,
                };
                let content = after.trim_start();
                let value_start = column + gap + (after.len() - content.len());
                let value = (!content.is_empty() && key_of(content).is_none()).then(|| Span {
                    line,
                    start: chars_before(raw, value_start),
                    end: chars_before(raw, value_start) + visible_len(content),
                });
                outline.entries.push(Entry {
                    path,
                    key: span,
                    value,
                });
                column += gap + (after.len() - content.len());
                rest = content;
                if rest.starts_with('|') || rest.starts_with('>') {
                    skip_deeper_than = Some(indent);
                }
            }

            if let Some((key, after_colon)) = key_of(rest) {
                while stack.last().is_some_and(|(at, _)| *at >= column) {
                    stack.pop();
                }
                stack.push((column, Segment::Key(key.text.clone())));
                let value_text = after_colon.trim_start();
                let value_col = column + (rest.len() - value_text.len());
                let value =
                    (!value_text.is_empty() && !value_text.starts_with('#')).then(|| Span {
                        line,
                        start: chars_before(raw, value_col),
                        end: chars_before(raw, value_col) + visible_len(value_text),
                    });
                outline.entries.push(Entry {
                    path: path_of(&stack),
                    key: Span {
                        line,
                        start: chars_before(raw, column),
                        end: chars_before(raw, column) + key.text.chars().count(),
                    },
                    value,
                });
                if value_text.starts_with('|') || value_text.starts_with('>') {
                    skip_deeper_than = Some(column.min(indent));
                } else if value_text.starts_with(['{', '[']) {
                    flow_depth = bracket_balance(value_text).max(0);
                }
            } else if rest.starts_with(['{', '[']) {
                flow_depth = bracket_balance(rest).max(0);
            }
        }
        outline
    }

    /// The span for `path`, or for its nearest ancestor that was found.
    pub fn find(&self, path: &[Segment], anchor: Anchor) -> Option<Span> {
        for length in (1..=path.len()).rev() {
            let wanted = path.get(..length)?;
            if let Some(entry) = self.entries.iter().find(|entry| entry.path == wanted) {
                return Some(match (anchor, entry.value) {
                    (Anchor::Value, Some(value)) if length == path.len() => value,
                    _ => entry.key,
                });
            }
        }
        None
    }
}

/// One document of a multi-document file and the line it starts on.
pub struct Chunk<'a> {
    pub first_line: usize,
    pub text: &'a str,
}

/// Cut a file at its `---` separators.
pub fn split_documents(yaml: &str) -> Vec<Chunk<'_>> {
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut first_line = 0usize;
    let mut position = 0usize;
    for (line, raw) in yaml.split_inclusive('\n').enumerate() {
        let is_separator = raw.starts_with("---")
            && raw
                .get(3..)
                .is_some_and(|tail| tail.trim().is_empty() || tail.starts_with([' ', '\t']));
        if is_separator {
            if let Some(text) = yaml.get(start..position) {
                chunks.push(Chunk { first_line, text });
            }
            start = position + raw.len();
            first_line = line + 1;
        }
        position += raw.len();
    }
    if let Some(text) = yaml.get(start..) {
        chunks.push(Chunk { first_line, text });
    }
    chunks
}

struct Key {
    text: String,
}

/// `key: rest` → the key and what follows the colon. A plain scalar that only
/// contains a colon, such as a URL, is not a key.
fn key_of(text: &str) -> Option<(Key, &str)> {
    if let Some(quote @ ('"' | '\'')) = text.chars().next() {
        let close = text.get(1..)?.find(quote)? + 1;
        let after = text.get(close + 1..)?;
        let after = after.strip_prefix(':')?;
        if !(after.is_empty() || after.starts_with([' ', '\t'])) {
            return None;
        }
        return Some((
            Key {
                text: text.get(1..close)?.to_string(),
            },
            after,
        ));
    }
    if text.starts_with(['{', '[', '#', '&', '*', '!', '|', '>']) {
        return None;
    }
    let mut search = 0;
    while let Some(found) = text.get(search..)?.find(':') {
        let at = search + found;
        let after = text.get(at + 1..)?;
        if after.is_empty() || after.starts_with([' ', '\t']) {
            return Some((
                Key {
                    text: text.get(..at)?.trim_end().to_string(),
                },
                after,
            ));
        }
        search = at + 1;
    }
    None
}

/// `- rest` (or a bare `-`) → what follows the dash.
fn strip_dash(text: &str) -> Option<&str> {
    let after = text.strip_prefix('-')?;
    (after.is_empty() || after.starts_with([' ', '\t'])).then_some(after)
}

/// A new item at `column` ends everything written deeper, and the previous item
/// at this column.
fn close_for_item(stack: &mut Vec<(usize, Segment)>, column: usize) {
    while stack.last().is_some_and(|(at, segment)| {
        *at > column || (*at == column && matches!(segment, Segment::Index(_)))
    }) {
        stack.pop();
    }
}

/// Which item this is: one past the last item closed at this level, else the
/// first. The last item is looked up in what was already indexed.
fn next_index(outline: &Outline, stack: &[(usize, Segment)]) -> usize {
    let parent = path_of(stack);
    outline
        .entries
        .iter()
        .filter_map(|entry| match entry.path.split_last() {
            Some((Segment::Index(index), head)) if head == parent.as_slice() => Some(*index + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn path_of(stack: &[(usize, Segment)]) -> Path {
    stack.iter().map(|(_, segment)| segment.clone()).collect()
}

fn chars_before(line: &str, byte: usize) -> usize {
    line.get(..byte).map_or(0, |head| head.chars().count())
}

/// Length of a value without a trailing comment.
fn visible_len(value: &str) -> usize {
    let end = value.find(" #").unwrap_or(value.len());
    value.get(..end).unwrap_or(value).trim_end().chars().count()
}

/// Net opened minus closed brackets on a line, ignoring quoted text.
fn bracket_balance(text: &str) -> i32 {
    let mut balance = 0;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{' | '[') => balance += 1,
            (None, '}' | ']') => balance -= 1,
            (None, '#') => break,
            _ => {}
        }
    }
    balance
}

#[cfg(test)]
mod tests {
    use super::{Outline, split_documents};
    use crate::validate::{Anchor, Segment};

    fn key(name: &str) -> Segment {
        Segment::Key(name.into())
    }

    const POD: &str = "\
apiVersion: v1
kind: Pod
metadata:
  name: web   # the name
  labels:
    app: web
spec:
  containers:
  - name: a
    image: nginx
    args:
    - -x
    - --y
  - name: b
    image: \"redis:7\"
    command: [\"sh\", \"-c\"]
";

    #[test]
    fn a_nested_key_is_found_with_its_key_and_value_spans() {
        let outline = Outline::build(POD, 0);
        let name = outline
            .find(&[key("metadata"), key("name")], Anchor::Key)
            .unwrap();
        assert_eq!((name.line, name.start, name.end), (3, 2, 6));
        let value = outline
            .find(&[key("metadata"), key("name")], Anchor::Value)
            .unwrap();
        assert_eq!((value.line, value.start, value.end), (3, 8, 11));
    }

    #[test]
    fn list_items_are_numbered_in_order_even_when_the_dash_shares_the_keys_column() {
        let outline = Outline::build(POD, 0);
        let first = [
            key("spec"),
            key("containers"),
            Segment::Index(0),
            key("image"),
        ];
        let second = [
            key("spec"),
            key("containers"),
            Segment::Index(1),
            key("image"),
        ];
        assert_eq!(outline.find(&first, Anchor::Value).unwrap().line, 9);
        assert_eq!(outline.find(&second, Anchor::Value).unwrap().line, 14);
        let arg = [
            key("spec"),
            key("containers"),
            Segment::Index(0),
            key("args"),
            Segment::Index(1),
        ];
        assert_eq!(outline.find(&arg, Anchor::Value).unwrap().line, 12);
    }

    #[test]
    fn a_quoted_value_keeps_its_quotes_in_the_span() {
        let outline = Outline::build(POD, 0);
        let image = [
            key("spec"),
            key("containers"),
            Segment::Index(1),
            key("image"),
        ];
        let span = outline.find(&image, Anchor::Value).unwrap();
        assert_eq!((span.start, span.end), (11, 20));
    }

    #[test]
    fn a_path_inside_flow_style_falls_back_to_the_line_that_holds_it() {
        let outline = Outline::build(POD, 0);
        let inside = [
            key("spec"),
            key("containers"),
            Segment::Index(1),
            key("command"),
            Segment::Index(0),
        ];
        assert_eq!(outline.find(&inside, Anchor::Value).unwrap().line, 15);
    }

    #[test]
    fn a_missing_path_falls_back_to_its_nearest_ancestor() {
        let outline = Outline::build(POD, 0);
        let span = outline
            .find(
                &[key("metadata"), key("annotations"), key("x")],
                Anchor::Key,
            )
            .unwrap();
        assert_eq!(span.line, 2);
    }

    #[test]
    fn block_scalars_and_comments_do_not_become_keys() {
        let text = "\
data:
  script: |
    echo: hi
    - not an item
  other: 1
# trailing: comment
";
        let outline = Outline::build(text, 0);
        assert!(
            outline
                .find(&[key("data"), key("other")], Anchor::Key)
                .unwrap()
                .line
                == 4
        );
        let fake = outline
            .find(&[key("data"), key("echo")], Anchor::Key)
            .unwrap();
        assert_eq!(fake.line, 0, "no such key, so the parent answers");
    }

    #[test]
    fn a_url_is_a_value_not_a_key() {
        let outline = Outline::build("a:\n  - http://x.example/y\n", 0);
        let item = outline
            .find(&[key("a"), Segment::Index(0)], Anchor::Value)
            .unwrap();
        assert_eq!((item.line, item.start), (1, 4));
    }

    #[test]
    fn documents_split_at_separators_and_keep_file_line_numbers() {
        let file = "a: 1\n---\nb: 2\nc: 3\n--- # next\nd: 4\n";
        let chunks = split_documents(file);
        let seen: Vec<(usize, &str)> = chunks.iter().map(|c| (c.first_line, c.text)).collect();
        assert_eq!(seen, [(0, "a: 1\n"), (2, "b: 2\nc: 3\n"), (5, "d: 4\n")]);
        let outline = Outline::build(chunks[1].text, chunks[1].first_line);
        assert_eq!(outline.find(&[key("c")], Anchor::Key).unwrap().line, 3);
    }
}
