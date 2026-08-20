//! The JSONPath subset Kubernetes printer columns actually use.
//!
//! `additionalPrinterColumns[].jsonPath` is evaluated by kubectl's own
//! restricted engine, not RFC 9535: paths start with a bare `.`, and the only
//! filter form in practice is `[?(@.field=="value")]`. A full RFC engine would
//! reject those inputs, so we implement the subset directly.
//!
//! Supported: `.a.b`, `$.a.b`, `['a']`, `[0]`, `[*]`, `.*`,
//! `[?(@.type=="Ready")]`, `[?(@.type=='Ready')]`.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Field(String),
    Index(usize),
    Wildcard,
    /// `[?(@.lhs == "rhs")]` — the only predicate kubectl-style paths use.
    Filter {
        lhs: String,
        rhs: String,
    },
}

/// A parsed path. Parse once per column, evaluate per row.
#[derive(Debug, Clone)]
pub struct JsonPath {
    steps: Vec<Step>,
    raw: String,
}

impl JsonPath {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let mut rest = expr.trim();
        rest = rest.strip_prefix('$').unwrap_or(rest);

        let mut steps = Vec::new();
        let bytes = rest.as_bytes();
        let mut i = 0usize;

        while i < bytes.len() {
            match bytes[i] {
                b'.' => {
                    i += 1;
                    // `..` (recursive descent) is not used by printer columns;
                    // rejecting it is better than silently mis-evaluating.
                    if i < bytes.len() && bytes[i] == b'.' {
                        return Err(format!("recursive descent is not supported: {expr}"));
                    }
                    if i < bytes.len() && bytes[i] == b'*' {
                        steps.push(Step::Wildcard);
                        i += 1;
                        continue;
                    }
                    let start = i;
                    while i < bytes.len() && !matches!(bytes[i], b'.' | b'[') {
                        i += 1;
                    }
                    if start == i {
                        continue; // trailing dot
                    }
                    steps.push(Step::Field(rest[start..i].to_string()));
                }
                b'[' => {
                    let close = rest[i..]
                        .find(']')
                        .ok_or_else(|| format!("unterminated `[` in {expr}"))?
                        + i;
                    let inner = rest[i + 1..close].trim();
                    steps.push(parse_bracket(inner, expr)?);
                    i = close + 1;
                }
                _ => {
                    // Bare leading segment, e.g. `metadata.name`.
                    let start = i;
                    while i < bytes.len() && !matches!(bytes[i], b'.' | b'[') {
                        i += 1;
                    }
                    steps.push(Step::Field(rest[start..i].to_string()));
                }
            }
        }

        Ok(Self {
            steps,
            raw: expr.to_string(),
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Every value the path selects. Empty when the path does not match —
    /// missing fields are normal (an unscheduled pod has no `.spec.nodeName`).
    pub fn eval<'a>(&self, root: &'a Value) -> Vec<&'a Value> {
        let mut current: Vec<&Value> = vec![root];
        for step in &self.steps {
            let mut next: Vec<&Value> = Vec::new();
            for value in current {
                match step {
                    Step::Field(name) => {
                        if let Some(v) = value.get(name.as_str()) {
                            next.push(v);
                        }
                    }
                    Step::Index(idx) => {
                        if let Some(v) = value.get(*idx) {
                            next.push(v);
                        }
                    }
                    Step::Wildcard => match value {
                        Value::Array(items) => next.extend(items.iter()),
                        Value::Object(map) => next.extend(map.values()),
                        _ => {}
                    },
                    Step::Filter { lhs, rhs } => {
                        let candidates: Vec<&Value> = match value {
                            Value::Array(items) => items.iter().collect(),
                            other => vec![other],
                        };
                        for item in candidates {
                            let matches = item
                                .get(lhs.as_str())
                                .map(|v| scalar_eq(v, rhs))
                                .unwrap_or(false);
                            if matches {
                                next.push(item);
                            }
                        }
                    }
                }
            }
            if next.is_empty() {
                return Vec::new();
            }
            current = next;
        }
        current
    }

    /// First match rendered the way kubectl prints it, or `None`.
    pub fn eval_display(&self, root: &Value) -> Option<String> {
        self.eval(root).first().map(|v| render(v))
    }
}

fn parse_bracket(inner: &str, expr: &str) -> Result<Step, String> {
    if inner == "*" {
        return Ok(Step::Wildcard);
    }
    if let Some(pred) = inner.strip_prefix('?') {
        let pred = pred
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim();
        let (lhs, rhs) = pred
            .split_once("==")
            .ok_or_else(|| format!("unsupported filter `{inner}` in {expr}"))?;
        let lhs = lhs.trim().trim_start_matches("@.").trim().to_string();
        let rhs = rhs.trim().trim_matches(['"', '\'']).to_string();
        return Ok(Step::Filter { lhs, rhs });
    }
    let unquoted = inner.trim_matches(['"', '\'']);
    if unquoted.len() != inner.len() {
        return Ok(Step::Field(unquoted.to_string()));
    }
    inner
        .parse::<usize>()
        .map(Step::Index)
        .map_err(|_| format!("unsupported bracket `{inner}` in {expr}"))
}

fn scalar_eq(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(s) => s == expected,
        Value::Bool(b) => b.to_string() == expected,
        Value::Number(n) => n.to_string() == expected,
        _ => false,
    }
}

/// Render a value for a table cell. Strings are shown bare (not JSON-quoted),
/// which is what kubectl does.
pub fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj() -> Value {
        json!({
            "metadata": {"name": "web", "labels": {"app": "web"}},
            "spec": {"replicas": 3, "containers": [{"name": "a"}, {"name": "b"}]},
            "status": {
                "conditions": [
                    {"type": "Available", "status": "True"},
                    {"type": "Progressing", "status": "False"}
                ]
            }
        })
    }

    #[test]
    fn reads_nested_field() {
        let p = JsonPath::parse(".metadata.name").unwrap();
        assert_eq!(p.eval_display(&obj()).as_deref(), Some("web"));
    }

    #[test]
    fn reads_number_without_quotes() {
        let p = JsonPath::parse(".spec.replicas").unwrap();
        assert_eq!(p.eval_display(&obj()).as_deref(), Some("3"));
    }

    #[test]
    fn indexes_into_arrays() {
        let p = JsonPath::parse(".spec.containers[1].name").unwrap();
        assert_eq!(p.eval_display(&obj()).as_deref(), Some("b"));
    }

    #[test]
    fn filters_conditions_by_type() {
        let p = JsonPath::parse(r#".status.conditions[?(@.type=="Available")].status"#).unwrap();
        assert_eq!(p.eval_display(&obj()).as_deref(), Some("True"));
    }

    #[test]
    fn bracketed_field_names_work() {
        let p = JsonPath::parse("['metadata']['labels']['app']").unwrap();
        assert_eq!(p.eval_display(&obj()).as_deref(), Some("web"));
    }

    #[test]
    fn wildcard_collects_all_matches() {
        let p = JsonPath::parse(".spec.containers[*].name").unwrap();
        let root = obj();
        let all: Vec<String> = p.eval(&root).iter().map(|v| render(v)).collect();
        assert_eq!(all, vec!["a", "b"]);
    }

    #[test]
    fn missing_field_yields_nothing() {
        let p = JsonPath::parse(".spec.nodeName").unwrap();
        assert!(p.eval_display(&obj()).is_none());
    }

    #[test]
    fn recursive_descent_is_rejected() {
        assert!(JsonPath::parse("..name").is_err());
    }
}
