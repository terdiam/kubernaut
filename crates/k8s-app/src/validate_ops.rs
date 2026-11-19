//! Checking a manifest against the schemas of the cluster it is meant for.
//!
//! Reads only: the cluster's OpenAPI document, cached per kind. A kind the
//! cluster does not serve, or one that publishes no schema, is left alone —
//! preview says so with the apiserver's own words.

use std::sync::Arc;

use k8s_core::{
    locate::{Outline, split_documents},
    schema::SchemaCache,
    validate::{render_path, validate},
};
use serde_json::Value;

use crate::{sources::ClusterSource, state::AppCore};

/// What checking a manifest needs of the app beyond a cluster: the cache of the
/// cluster's resource schemas.
pub trait SchemaSource: ClusterSource {
    fn schemas(&self) -> &SchemaCache;
}

impl SchemaSource for AppCore {
    fn schemas(&self) -> &SchemaCache {
        &self.schemas
    }
}

impl<H: SchemaSource + ?Sized> SchemaSource for Arc<H> {
    fn schemas(&self) -> &SchemaCache {
        (**self).schemas()
    }
}

/// One thing wrong with the text, placed on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Zero-based, in the whole text.
    pub line: usize,
    /// Characters, `start..end` on that line.
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// Everything wrong with `yaml`: text that is not YAML, and fields the schema of
/// each document's kind does not allow.
pub async fn validate_yaml(core: &impl SchemaSource, cluster: &str, yaml: &str) -> Vec<Problem> {
    let Ok(handle) = core.handle(cluster) else {
        return Vec::new();
    };
    let mut problems = Vec::new();

    for chunk in split_documents(yaml) {
        let value: Value = match serde_yaml_ng::from_str(chunk.text) {
            Ok(Value::Null) => continue,
            Ok(value) => value,
            Err(err) => {
                // A syntax error is the one problem worth showing alone: the
                // rest of the document cannot be trusted to mean anything.
                let (line, column) = err
                    .location()
                    .map_or((0, 0), |at| (at.line().saturating_sub(1), at.column()));
                problems.push(Problem {
                    line: chunk.first_line + line,
                    start: column.saturating_sub(1),
                    end: column,
                    message: format!("not valid YAML: {}", first_line_of(&err.to_string())),
                });
                continue;
            }
        };

        let (Some(api_version), Some(kind)) = (
            value.get("apiVersion").and_then(Value::as_str),
            value.get("kind").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(key) = handle.discovery().and_then(|discovery| {
            discovery
                .resolve_gvk(api_version, kind)
                .map(|d| d.key.clone())
        }) else {
            continue;
        };
        let Ok(schema) = core.schemas().get(&handle, &key).await else {
            continue;
        };

        let outline = Outline::build(chunk.text, chunk.first_line);
        for issue in validate(&schema.schema, &value) {
            let span = outline
                .find(&issue.path, issue.anchor)
                .unwrap_or(k8s_core::locate::Span {
                    line: chunk.first_line,
                    start: 0,
                    end: 0,
                });
            let message = if issue.path.is_empty() {
                issue.message
            } else {
                format!("{}: {}", render_path(&issue.path), issue.message)
            };
            problems.push(Problem {
                line: span.line,
                start: span.start,
                end: span.end,
                message,
            });
        }
    }
    problems
}

/// serde_yaml_ng errors end with the position; the editor shows that itself.
fn first_line_of(message: &str) -> &str {
    let line = message.lines().next().unwrap_or(message);
    line.rsplit_once(" at line ").map_or(line, |(head, _)| head)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use k8s_core::{cluster::ClusterHandle, schema::SchemaCache};

    use super::{SchemaSource, first_line_of, validate_yaml};
    use crate::sources::ClusterSource;

    /// An app with no clusters, and an empty schema cache.
    #[derive(Default)]
    struct NoClusters {
        schemas: SchemaCache,
    }

    impl ClusterSource for NoClusters {
        fn handle(&self, cluster: &str) -> Result<Arc<ClusterHandle>, String> {
            Err(format!("no cluster `{cluster}`"))
        }
    }

    impl SchemaSource for NoClusters {
        fn schemas(&self) -> &SchemaCache {
            &self.schemas
        }
    }

    #[tokio::test]
    async fn a_manifest_for_a_cluster_the_app_does_not_have_is_left_unchecked() {
        let problems = validate_yaml(
            &NoClusters::default(),
            "prod",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n",
        )
        .await;
        assert!(problems.is_empty(), "nothing to check it against");
    }

    #[test]
    fn the_position_is_dropped_from_a_yaml_error() {
        assert_eq!(
            first_line_of("did not find expected key at line 3 column 1"),
            "did not find expected key"
        );
        assert_eq!(first_line_of("plain"), "plain");
    }
}
