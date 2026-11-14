//! Check a document against the JSON Schema a cluster publishes for its kind.
//!
//! The apiserver's own dry-run says the same things, but only at preview time
//! and one document at a time. A local check can run on every pause in typing,
//! works against a protected or slow cluster, and points at a path.
//!
//! This is not a general JSON Schema engine. It covers what Kubernetes schemas
//! use — types, `required`, `enum`, `properties`, `additionalProperties`,
//! `items`, `$ref`, `allOf` and the `x-kubernetes-*` markers that loosen a node
//! — and stays silent about anything else, because a false error in an editor
//! is worse than a missed one that the apiserver still catches.

use serde_json::{Map, Value};

/// Issues kept per document; a badly wrong file would otherwise bury the first.
const MAX_ISSUES: usize = 50;
/// Guard against a schema nested deeper than any real kind.
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    Key(String),
    Index(usize),
}

pub type Path = Vec<Segment>;

/// What in the text the problem is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The field name: it is unknown, or something is missing under it.
    Key,
    /// The field's value: it has the wrong type or an unlisted value.
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: Path,
    pub anchor: Anchor,
    pub message: String,
}

/// Dotted form for messages, such as `spec.containers[0].image`.
pub fn render_path(path: &[Segment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Segment::Index(index) => {
                out.push('[');
                out.push_str(&index.to_string());
                out.push(']');
            }
        }
    }
    out
}

/// Everything wrong with `document` according to `schema`, as built by
/// [`crate::schema`], so every `$ref` points at `#/$defs/<name>`.
pub fn validate(schema: &Value, document: &Value) -> Vec<Issue> {
    let mut checker = Checker {
        defs: schema.get("$defs").and_then(Value::as_object),
        path: Vec::new(),
        issues: Vec::new(),
    };
    checker.check(schema, document, 0);
    checker.issues
}

struct Checker<'a> {
    defs: Option<&'a Map<String, Value>>,
    path: Path,
    issues: Vec<Issue>,
}

impl Checker<'_> {
    fn report(&mut self, anchor: Anchor, message: String) {
        if self.issues.len() < MAX_ISSUES {
            self.issues.push(Issue {
                path: self.path.clone(),
                anchor,
                message,
            });
        }
    }

    fn check(&mut self, schema: &Value, value: &Value, depth: usize) {
        if depth > MAX_DEPTH || self.issues.len() >= MAX_ISSUES {
            return;
        }
        let Some(schema) = schema.as_object() else {
            return;
        };

        if let Some(name) = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|target| target.strip_prefix("#/$defs/"))
        {
            // The apiserver reads a bare number as a quantity, so `cpu: 1` is
            // as valid as `cpu: "1"` although the schema says string.
            if name.ends_with("resource.Quantity") && value.is_number() {
                return;
            }
            if let Some(target) = self.defs.and_then(|defs| defs.get(name)) {
                self.check(target, value, depth + 1);
            }
        }
        if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
            for part in parts {
                self.check(part, value, depth + 1);
            }
        }
        // An explicit null is how a manifest clears a field; the apiserver
        // drops it rather than rejecting it.
        if value.is_null() {
            return;
        }

        let int_or_string = schema.get("x-kubernetes-int-or-string") == Some(&Value::Bool(true))
            || schema.get("format").and_then(Value::as_str) == Some("int-or-string");
        if int_or_string {
            if !(value.is_number() || value.is_string()) {
                self.report(
                    Anchor::Value,
                    format!("must be an integer or a string, not {}", describe(value)),
                );
            }
            return;
        }

        match schema.get("type").and_then(Value::as_str) {
            Some("object") => self.check_object(schema, value, depth),
            Some("array") => self.check_array(schema, value, depth),
            Some("string") => {
                if value.is_string() {
                    self.check_enum(schema, value);
                } else {
                    self.report(
                        Anchor::Value,
                        format!("must be a string, not {}", describe(value)),
                    );
                }
            }
            Some("integer") if !is_integer(value) => self.report(
                Anchor::Value,
                format!("must be an integer, not {}", describe(value)),
            ),
            Some("number") if !value.is_number() => self.report(
                Anchor::Value,
                format!("must be a number, not {}", describe(value)),
            ),
            Some("boolean") if !value.is_boolean() => self.report(
                Anchor::Value,
                format!("must be true or false, not {}", describe(value)),
            ),
            _ => {}
        }
    }

    fn check_object(&mut self, schema: &Map<String, Value>, value: &Value, depth: usize) {
        let Some(object) = value.as_object() else {
            self.report(
                Anchor::Value,
                format!("must be an object, not {}", describe(value)),
            );
            return;
        };
        let properties = schema.get("properties").and_then(Value::as_object);
        let preserve =
            schema.get("x-kubernetes-preserve-unknown-fields") == Some(&Value::Bool(true));

        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    self.report(Anchor::Key, format!("missing required field `{name}`"));
                }
            }
        }

        for (key, child) in object {
            self.path.push(Segment::Key(key.clone()));
            match (
                properties.and_then(|p| p.get(key)),
                schema.get("additionalProperties"),
            ) {
                (Some(known), _) => self.check(known, child, depth + 1),
                (None, Some(extra @ Value::Object(_))) => self.check(extra, child, depth + 1),
                // A map with no declared fields holds whatever it likes.
                (None, _) if preserve || properties.is_none_or(Map::is_empty) => {}
                (None, Some(Value::Bool(true))) => {}
                (None, _) => self.report(Anchor::Key, format!("unknown field `{key}`")),
            }
            self.path.pop();
        }
    }

    fn check_array(&mut self, schema: &Map<String, Value>, value: &Value, depth: usize) {
        let Some(items) = value.as_array() else {
            self.report(
                Anchor::Value,
                format!("must be a list, not {}", describe(value)),
            );
            return;
        };
        let Some(item_schema) = schema.get("items") else {
            return;
        };
        for (index, item) in items.iter().enumerate() {
            self.path.push(Segment::Index(index));
            self.check(item_schema, item, depth + 1);
            self.path.pop();
        }
    }

    fn check_enum(&mut self, schema: &Map<String, Value>, value: &Value) {
        let Some(allowed) = schema.get("enum").and_then(Value::as_array) else {
            return;
        };
        if allowed.contains(value) {
            return;
        }
        let list: Vec<&str> = allowed.iter().filter_map(Value::as_str).collect();
        self.report(
            Anchor::Value,
            format!("must be one of: {}", list.join(", ")),
        );
    }
}

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.is_i64() || number.is_u64() || number.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Anchor, Issue, Segment, render_path, validate};

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["apiVersion", "kind"],
            "properties": {
                "apiVersion": {"type": "string"},
                "kind": {"type": "string"},
                "metadata": {"$ref": "#/$defs/Meta"},
                "spec": {
                    "type": "object",
                    "required": ["replicas"],
                    "properties": {
                        "replicas": {"type": "integer"},
                        "policy": {"type": "string", "enum": ["Always", "Never"]},
                        "port": {"x-kubernetes-int-or-string": true},
                        "limits": {"$ref": "#/$defs/io.k8s.apimachinery.pkg.api.resource.Quantity"},
                        "containers": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {"image": {"type": "string"}}
                            }
                        }
                    }
                }
            },
            "$defs": {
                "Meta": {
                    "type": "object",
                    "properties": {
                        "labels": {
                            "type": "object",
                            "additionalProperties": {"type": "string"}
                        }
                    }
                },
                "io.k8s.apimachinery.pkg.api.resource.Quantity": {"type": "string"}
            }
        })
    }

    fn issues(document: serde_json::Value) -> Vec<Issue> {
        validate(&schema(), &document)
    }

    fn messages(document: serde_json::Value) -> Vec<String> {
        issues(document).into_iter().map(|i| i.message).collect()
    }

    #[test]
    fn a_valid_document_has_no_issues() {
        assert!(
            issues(json!({
                "apiVersion": "v1", "kind": "X",
                "metadata": {"labels": {"a": "b"}},
                "spec": {"replicas": 3, "policy": "Always", "port": "http",
                         "containers": [{"image": "nginx"}]}
            }))
            .is_empty()
        );
    }

    #[test]
    fn a_misspelt_field_is_unknown_and_anchored_to_its_key() {
        let found = issues(json!({
            "apiVersion": "v1", "kind": "X",
            "spec": {"replicas": 1, "replicaz": 2}
        }));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "unknown field `replicaz`");
        assert_eq!(found[0].anchor, Anchor::Key);
        assert_eq!(render_path(&found[0].path), "spec.replicaz");
    }

    #[test]
    fn a_missing_required_field_names_it() {
        assert_eq!(
            messages(json!({"apiVersion": "v1", "kind": "X", "spec": {}})),
            ["missing required field `replicas`"]
        );
        assert_eq!(
            messages(json!({"kind": "X"})),
            ["missing required field `apiVersion`"]
        );
    }

    #[test]
    fn wrong_types_say_what_was_found() {
        let mut found = messages(json!({
            "apiVersion": "v1", "kind": "X",
            "spec": {"replicas": "3", "containers": {"image": "x"}}
        }));
        found.sort();
        assert_eq!(
            found,
            [
                "must be a list, not an object",
                "must be an integer, not a string"
            ]
        );
    }

    #[test]
    fn a_number_where_a_string_is_wanted_is_an_error_but_not_for_a_quantity() {
        assert_eq!(
            messages(json!({
                "apiVersion": "v1", "kind": "X", "metadata": {"labels": {"n": 1}},
                "spec": {"replicas": 1, "limits": 2}
            })),
            ["must be a string, not a number"]
        );
    }

    #[test]
    fn an_enum_lists_what_is_allowed() {
        assert_eq!(
            messages(json!({
                "apiVersion": "v1", "kind": "X", "spec": {"replicas": 1, "policy": "Sometimes"}
            })),
            ["must be one of: Always, Never"]
        );
    }

    #[test]
    fn int_or_string_takes_either_but_not_a_list() {
        assert!(
            messages(json!({
                "apiVersion": "v1", "kind": "X", "spec": {"replicas": 1, "port": 80}
            }))
            .is_empty()
        );
        assert_eq!(
            messages(json!({
                "apiVersion": "v1", "kind": "X", "spec": {"replicas": 1, "port": [1]}
            })),
            ["must be an integer or a string, not a list"]
        );
    }

    #[test]
    fn null_clears_a_field_and_a_whole_float_is_an_integer() {
        assert!(
            issues(json!({
                "apiVersion": "v1", "kind": "X", "spec": {"replicas": 2.0, "policy": null}
            }))
            .is_empty()
        );
    }

    #[test]
    fn preserve_unknown_fields_and_free_form_maps_accept_anything() {
        let schema = json!({
            "type": "object",
            "properties": {
                "free": {"type": "object"},
                "loose": {
                    "type": "object",
                    "x-kubernetes-preserve-unknown-fields": true,
                    "properties": {"a": {"type": "string"}}
                }
            }
        });
        let document = json!({"free": {"x": 1}, "loose": {"a": "s", "anything": [1]}});
        assert!(validate(&schema, &document).is_empty());
    }

    #[test]
    fn paths_render_with_indexes() {
        let path = vec![
            Segment::Key("spec".into()),
            Segment::Key("containers".into()),
            Segment::Index(0),
            Segment::Key("image".into()),
        ];
        assert_eq!(render_path(&path), "spec.containers[0].image");
    }

    #[test]
    fn a_ref_cycle_in_the_document_depth_terminates() {
        let schema = json!({
            "$defs": {"N": {"type": "object", "properties": {"next": {"$ref": "#/$defs/N"}}}},
            "$ref": "#/$defs/N"
        });
        let mut document = json!({});
        for _ in 0..200 {
            document = json!({"next": document});
        }
        assert!(validate(&schema, &document).is_empty());
    }
}
