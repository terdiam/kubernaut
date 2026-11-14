//! Form layouts for creating the kinds people create most, and the manifest a
//! filled-in form makes.
//!
//! The cluster's schema says what a field is, not which few matter. These
//! layouts are that editorial judgement for common kinds; anything without one
//! is still created from YAML, so no kind is out of reach. Nothing here knows
//! about a window: a layout is data, and building a manifest from values is a
//! plain function, so both are tested without a screen.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// An object as the forms read and write it, so a front end can hold one
/// without depending on a JSON crate of its own.
pub use serde_json::Value as Object;

/// What a field holds, and so how it is drawn and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    /// A port number, or a port's name: `8080` or `http`.
    Port,
    Select(&'static [&'static str]),
    /// Key and value rows: an object (`labels`), or a list of `{name, value}`
    /// (`env`) when `list` is set.
    Pairs {
        list: bool,
    },
    /// A Secret's values, as key and value rows of plain text. A new Secret
    /// writes them to `stringData`, which the apiserver encodes. An existing one
    /// holds them base64-encoded in `data` and never returns `stringData`, so
    /// editing reads `data` and writes `data`. Values the rows cannot hold, such
    /// as a certificate with line breaks or binary content, are not listed and
    /// are left as they are.
    Secret,
    /// Words split on whitespace, written as a list: a command.
    Words,
    /// Free text with suggestions read from the cluster. `depends_on` names the
    /// field whose value narrows them. With `numeric` the value is a port: a
    /// number goes to the field's `number`, and a name (which a named service
    /// port is offered as) to its `name`.
    Lookup {
        source: &'static str,
        depends_on: Option<&'static str>,
        numeric: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Identifies the field in the values of a form.
    pub key: &'static str,
    pub label: &'static str,
    /// Where the value goes in the manifest: dotted, with a number for a list
    /// element (`spec.ports.0.port`).
    pub path: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    pub placeholder: &'static str,
    pub required: bool,
    /// What the field starts with.
    pub default: &'static str,
}

impl Field {
    const fn new(key: &'static str, label: &'static str, path: &'static str, kind: Kind) -> Self {
        Self {
            key,
            label,
            path,
            kind,
            help: "",
            placeholder: "",
            required: false,
            default: "",
        }
    }

    const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    const fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }

    const fn hint(mut self, placeholder: &'static str) -> Self {
        self.placeholder = placeholder;
        self
    }

    const fn start(mut self, default: &'static str) -> Self {
        self.default = default;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub title: &'static str,
    pub fields: &'static [Field],
}

#[derive(Debug, Clone, Copy)]
pub struct FormSpec {
    pub kind: &'static str,
    pub api_version: &'static str,
    /// `group/version/plural`, for the lookups a field makes.
    pub resource: &'static str,
    pub namespaced: bool,
    pub sections: &'static [Section],
}

impl FormSpec {
    pub fn fields(&self) -> impl Iterator<Item = &'static Field> + '_ {
        self.sections
            .iter()
            .flat_map(|section| section.fields.iter())
    }
}

/// A field's current value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    Pairs(Vec<(String, String)>),
}

impl FieldValue {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Pairs(pairs) => pairs.iter().all(|(key, _)| key.trim().is_empty()),
        }
    }
}

/// The values of a form, by field key.
pub type Values = BTreeMap<&'static str, FieldValue>;

// ------------------------------------------------------------------ layouts

const NAME: Field = Field::new("name", "Name", "metadata.name", Kind::Text)
    .required()
    .hint("example");
const NAMESPACE: Field = Field::new("namespace", "Namespace", "metadata.namespace", Kind::Text)
    .required()
    .hint("default");
const LABELS: Field = Field::new(
    "labels",
    "Labels",
    "metadata.labels",
    Kind::Pairs { list: false },
);

const ACCESS_MODES: &[&str] = &[
    "ReadWriteOnce",
    "ReadOnlyMany",
    "ReadWriteMany",
    "ReadWriteOncePod",
];

pub const SPECS: &[FormSpec] = &[
    FormSpec {
        kind: "Deployment",
        api_version: "apps/v1",
        resource: "apps/v1/deployments",
        namespaced: true,
        sections: &[
            Section {
                title: "Basics",
                fields: &[
                    NAME,
                    NAMESPACE,
                    Field::new("replicas", "Replicas", "spec.replicas", Kind::Number).start("2"),
                    LABELS,
                ],
            },
            Section {
                title: "Container",
                fields: &[
                    Field::new(
                        "image",
                        "Image",
                        "spec.template.spec.containers.0.image",
                        Kind::Text,
                    )
                    .required()
                    .hint("nginx:1.27"),
                    Field::new(
                        "container",
                        "Container name",
                        "spec.template.spec.containers.0.name",
                        Kind::Text,
                    )
                    .start("app"),
                    Field::new(
                        "port",
                        "Container port",
                        "spec.template.spec.containers.0.ports.0.containerPort",
                        Kind::Number,
                    )
                    .hint("8080"),
                    Field::new(
                        "env",
                        "Environment",
                        "spec.template.spec.containers.0.env",
                        Kind::Pairs { list: true },
                    )
                    .help("For a secret value, reference a Secret in YAML instead of typing it here."),
                ],
            },
            Section {
                title: "Resources",
                fields: &[
                    Field::new(
                        "cpu_request",
                        "CPU request",
                        "spec.template.spec.containers.0.resources.requests.cpu",
                        Kind::Text,
                    )
                    .hint("100m")
                    .help("What the scheduler reserves. Leave blank to reserve nothing."),
                    Field::new(
                        "memory_request",
                        "Memory request",
                        "spec.template.spec.containers.0.resources.requests.memory",
                        Kind::Text,
                    )
                    .hint("128Mi"),
                    Field::new(
                        "memory_limit",
                        "Memory limit",
                        "spec.template.spec.containers.0.resources.limits.memory",
                        Kind::Text,
                    )
                    .hint("256Mi")
                    .help("The container is killed past this. No CPU limit is offered on purpose: it throttles rather than frees capacity."),
                ],
            },
            Section {
                title: "Pod",
                fields: &[
                    Field::new(
                        "pull_secret",
                        "Image pull secret",
                        "spec.template.spec.imagePullSecrets.0.name",
                        Kind::Lookup {
                            source: "dockerConfigSecrets",
                            depends_on: None,
                            numeric: false,
                        },
                    ),
                    Field::new(
                        "service_account",
                        "Service account",
                        "spec.template.spec.serviceAccountName",
                        Kind::Lookup {
                            source: "serviceAccounts",
                            depends_on: None,
                            numeric: false,
                        },
                    ),
                ],
            },
        ],
    },
    FormSpec {
        kind: "Service",
        api_version: "v1",
        resource: "core/v1/services",
        namespaced: true,
        sections: &[
            Section {
                title: "Basics",
                fields: &[
                    NAME,
                    NAMESPACE,
                    Field::new(
                        "type",
                        "Type",
                        "spec.type",
                        Kind::Select(&["ClusterIP", "NodePort", "LoadBalancer"]),
                    )
                    .start("ClusterIP"),
                    LABELS,
                ],
            },
            Section {
                title: "Routing",
                fields: &[
                    Field::new("selector", "Selector", "spec.selector", Kind::Pairs { list: false })
                        .help("The pods it sends traffic to, by label. A selector that matches nothing leaves the service with no endpoints."),
                    Field::new("port", "Port", "spec.ports.0.port", Kind::Number)
                        .required()
                        .start("80"),
                    Field::new("target_port", "Target port", "spec.ports.0.targetPort", Kind::Port)
                        .hint("8080 or http")
                        .help("The port on the pod. Blank means the same as the port."),
                    Field::new("port_name", "Port name", "spec.ports.0.name", Kind::Text).hint("http"),
                ],
            },
        ],
    },
    FormSpec {
        kind: "ConfigMap",
        api_version: "v1",
        resource: "core/v1/configmaps",
        namespaced: true,
        sections: &[Section {
            title: "Basics",
            fields: &[
                NAME,
                NAMESPACE,
                LABELS,
                Field::new("data", "Data", "data", Kind::Pairs { list: false })
                    .help("One line per value. For a multi-line file, use YAML."),
            ],
        }],
    },
    FormSpec {
        kind: "Secret",
        api_version: "v1",
        resource: "core/v1/secrets",
        namespaced: true,
        sections: &[Section {
            title: "Basics",
            fields: &[
                NAME,
                NAMESPACE,
                Field::new(
                    "type",
                    "Type",
                    "type",
                    Kind::Select(&["Opaque", "kubernetes.io/basic-auth", "kubernetes.io/ssh-auth"]),
                )
                .start("Opaque"),
                LABELS,
                Field::new("string_data", "Values", "stringData", Kind::Secret).help(
                    "Plain text; the apiserver stores it base64-encoded. A value with line \
                     breaks, such as a certificate or a key, or with binary content, is not \
                     listed here: edit it in YAML, and it is kept as it is.",
                ),
            ],
        }],
    },
    FormSpec {
        kind: "Ingress",
        api_version: "networking.k8s.io/v1",
        resource: "networking.k8s.io/v1/ingresses",
        namespaced: true,
        sections: &[
            Section {
                title: "Basics",
                fields: &[
                    NAME,
                    NAMESPACE,
                    Field::new(
                        "class",
                        "Ingress class",
                        "spec.ingressClassName",
                        Kind::Lookup {
                            source: "ingressClasses",
                            depends_on: None,
                            numeric: false,
                        },
                    ),
                    LABELS,
                ],
            },
            Section {
                title: "Rule",
                fields: &[
                    Field::new("host", "Host", "spec.rules.0.host", Kind::Text).hint("app.example.com"),
                    Field::new("path", "Path", "spec.rules.0.http.paths.0.path", Kind::Text)
                        .start("/"),
                    Field::new(
                        "path_type",
                        "Path type",
                        "spec.rules.0.http.paths.0.pathType",
                        Kind::Select(&["Prefix", "Exact", "ImplementationSpecific"]),
                    )
                    .start("Prefix"),
                    Field::new(
                        "service",
                        "Backend service",
                        "spec.rules.0.http.paths.0.backend.service.name",
                        Kind::Lookup {
                            source: "services",
                            depends_on: None,
                            numeric: false,
                        },
                    )
                    .required(),
                    Field::new(
                        "service_port",
                        "Backend port",
                        "spec.rules.0.http.paths.0.backend.service.port.number",
                        Kind::Lookup {
                            source: "servicePorts",
                            depends_on: Some("service"),
                            numeric: true,
                        },
                    )
                    .required(),
                ],
            },
        ],
    },
    FormSpec {
        kind: "PersistentVolumeClaim",
        api_version: "v1",
        resource: "core/v1/persistentvolumeclaims",
        namespaced: true,
        sections: &[Section {
            title: "Basics",
            fields: &[
                NAME,
                NAMESPACE,
                Field::new("size", "Size", "spec.resources.requests.storage", Kind::Text)
                    .required()
                    .start("1Gi"),
                Field::new("mode", "Access mode", "spec.accessModes.0", Kind::Select(ACCESS_MODES))
                    .start("ReadWriteOnce"),
                Field::new(
                    "class",
                    "Storage class",
                    "spec.storageClassName",
                    Kind::Lookup {
                        source: "storageClasses",
                        depends_on: None,
                        numeric: false,
                    },
                )
                .help("Blank uses the cluster's default."),
                LABELS,
            ],
        }],
    },
    FormSpec {
        kind: "CronJob",
        api_version: "batch/v1",
        resource: "batch/v1/cronjobs",
        namespaced: true,
        sections: &[
            Section {
                title: "Schedule",
                fields: &[
                    NAME,
                    NAMESPACE,
                    Field::new("schedule", "Schedule", "spec.schedule", Kind::Text)
                        .required()
                        .start("0 2 * * *")
                        .help("Five cron fields: minute, hour, day of month, month, day of week."),
                    Field::new(
                        "concurrency",
                        "Concurrency",
                        "spec.concurrencyPolicy",
                        Kind::Select(&["Forbid", "Allow", "Replace"]),
                    )
                    .start("Forbid"),
                ],
            },
            Section {
                title: "Job",
                fields: &[
                    Field::new(
                        "image",
                        "Image",
                        "spec.jobTemplate.spec.template.spec.containers.0.image",
                        Kind::Text,
                    )
                    .required()
                    .hint("busybox:1.36"),
                    Field::new(
                        "command",
                        "Command",
                        "spec.jobTemplate.spec.template.spec.containers.0.command",
                        Kind::Words,
                    )
                    .hint("sh -c 'echo done'"),
                    Field::new(
                        "restart",
                        "Restart policy",
                        "spec.jobTemplate.spec.template.spec.restartPolicy",
                        Kind::Select(&["Never", "OnFailure"]),
                    )
                    .start("Never"),
                ],
            },
        ],
    },
    FormSpec {
        kind: "Namespace",
        api_version: "v1",
        resource: "core/v1/namespaces",
        namespaced: false,
        sections: &[Section {
            title: "Basics",
            fields: &[NAME, LABELS],
        }],
    },
];

// ------------------------------------------------------------------ building

/// Put a value at a dotted path, making the objects and lists on the way. A
/// number in the path is a list index.
fn set_path(root: &mut Value, path: &str, value: Value) {
    let mut cursor = root;
    for segment in path.split('.') {
        match segment.parse::<usize>() {
            Ok(index) => {
                if !cursor.is_array() {
                    *cursor = Value::Array(Vec::new());
                }
                if let Value::Array(list) = cursor {
                    if list.len() <= index {
                        list.resize(index + 1, Value::Null);
                    }
                    cursor = &mut list[index];
                }
            }
            Err(_) => {
                if !cursor.is_object() {
                    *cursor = Value::Object(Map::new());
                }
                if let Value::Object(map) = cursor {
                    cursor = map.entry(segment).or_insert(Value::Null);
                }
            }
        }
    }
    *cursor = value;
}

/// A port as it is written: a number when it is one, else a name.
fn port_value(text: &str) -> Value {
    text.parse::<i64>()
        .map_or_else(|_| json!(text), |number| json!(number))
}

fn field_json(field: &Field, value: &FieldValue) -> Option<Value> {
    if value.is_empty() {
        return None;
    }
    Some(match (field.kind, value) {
        (Kind::Number, FieldValue::Text(text)) => json!(text.trim().parse::<i64>().ok()?),
        (Kind::Lookup { numeric: true, .. }, FieldValue::Text(text)) => port_value(text.trim()),
        (Kind::Port, FieldValue::Text(text)) => port_value(text.trim()),
        (Kind::Words, FieldValue::Text(text)) => {
            json!(text.split_whitespace().collect::<Vec<_>>())
        }
        (Kind::Pairs { list: true }, FieldValue::Pairs(pairs)) => Value::Array(
            pairs
                .iter()
                .filter(|(key, _)| !key.trim().is_empty())
                .map(|(key, value)| json!({"name": key.trim(), "value": value}))
                .collect(),
        ),
        (Kind::Pairs { list: false } | Kind::Secret, FieldValue::Pairs(pairs)) => Value::Object(
            pairs
                .iter()
                .filter(|(key, _)| !key.trim().is_empty())
                .map(|(key, value)| (key.trim().to_string(), json!(value)))
                .collect(),
        ),
        (_, FieldValue::Text(text)) => json!(text.trim()),
        (_, FieldValue::Pairs(_)) => return None,
    })
}

/// The manifest a form describes. A blank field is left out rather than written
/// as an empty string, so the cluster's own default applies.
pub fn build(spec: &FormSpec, values: &Values) -> Value {
    let mut manifest = json!({"apiVersion": spec.api_version, "kind": spec.kind});
    for field in spec.fields() {
        if let Some(value) = values
            .get(field.key)
            .and_then(|value| field_json(field, value))
        {
            // A port that is a name, not a number, is written to `name`.
            let path = match (field.kind, &value) {
                (Kind::Lookup { numeric: true, .. }, Value::String(_)) => field
                    .path
                    .strip_suffix("number")
                    .map_or_else(|| field.path.to_string(), |stem| format!("{stem}name")),
                _ => field.path.to_string(),
            };
            set_path(&mut manifest, &path, value);
        }
    }
    derive(spec, values, &mut manifest);
    manifest
}

/// What a workload needs to be coherent, so a person is not asked to repeat it:
/// the selector and the pod labels must agree, and they follow the name.
fn derive(spec: &FormSpec, values: &Values, manifest: &mut Value) {
    if spec.kind != "Deployment" {
        return;
    }
    let name = match values.get("name") {
        Some(FieldValue::Text(name)) if !name.trim().is_empty() => name.trim().to_string(),
        _ => return,
    };
    set_path(manifest, "spec.selector.matchLabels.app", json!(name));
    set_path(manifest, "spec.template.metadata.labels.app", json!(name));
    if manifest.pointer("/metadata/labels/app").is_none() {
        set_path(manifest, "metadata.labels.app", json!(name));
    }
}

/// An object from YAML text, for reading its fields into a form.
pub fn parse_yaml(text: &str) -> Result<Value, String> {
    let value: Value = serde_yaml_ng::from_str(text).map_err(|err| err.to_string())?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("That is not a Kubernetes object.".to_string())
    }
}

/// The value at a dotted path, where a number is a list index.
fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = root;
    for segment in path.split('.') {
        cursor = match segment.parse::<usize>() {
            Ok(index) => cursor.get(index)?,
            Err(_) => cursor.get(segment)?,
        };
    }
    Some(cursor)
}

/// Remove the value at a dotted path, and the objects it leaves empty, so an
/// emptied field does not leave `resources: {requests: {}}` behind.
fn remove_path(root: &mut Value, path: &str) {
    fn go(value: &mut Value, segments: &[&str]) {
        let Some((first, rest)) = segments.split_first() else {
            return;
        };
        let child = match first.parse::<usize>() {
            Ok(index) => value.get_mut(index),
            Err(_) => value.get_mut(*first),
        };
        let Some(child) = child else { return };
        if rest.is_empty() {
            if let (Value::Object(map), false) = (&mut *value, first.parse::<usize>().is_ok()) {
                map.remove(*first);
            }
            return;
        }
        go(child, rest);
        // Tidy an object this left empty, but never a list element: removing
        // one would renumber the rest.
        if let Value::Object(map) = value
            && map
                .get(*first)
                .is_some_and(|v| v.as_object().is_some_and(Map::is_empty))
        {
            map.remove(*first);
        }
    }
    go(root, &path.split('.').collect::<Vec<_>>());
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// What a form shows for an existing object: the value at each field's path.
/// A field the object does not set is blank, as it is when creating.
pub fn read_values(spec: &FormSpec, object: &Value) -> Values {
    let mut values = Values::new();
    for field in spec.fields() {
        let at = get_path(object, field.path);
        let value = match field.kind {
            Kind::Pairs { list: false } => FieldValue::Pairs(
                at.and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .map(|(key, value)| (key.clone(), text_of(value).unwrap_or_default()))
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            Kind::Pairs { list: true } => FieldValue::Pairs(
                at.and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            // An entry that takes its value from a Secret or a
                            // ConfigMap has none to show; it is kept as it is.
                            .filter_map(|entry| {
                                Some((
                                    entry.get("name")?.as_str()?.to_string(),
                                    entry.get("value").and_then(text_of)?,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            Kind::Secret => FieldValue::Pairs(secret_rows(object)),
            Kind::Words => FieldValue::Text(
                at.and_then(Value::as_array)
                    .map(|words| {
                        words
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            ),
            Kind::Lookup { numeric: true, .. } => FieldValue::Text(
                at.and_then(text_of)
                    .or_else(|| {
                        // A backend port set by name.
                        let named = field.path.strip_suffix("number")?;
                        get_path(object, &format!("{named}name")).and_then(text_of)
                    })
                    .unwrap_or_default(),
            ),
            _ => FieldValue::Text(at.and_then(text_of).unwrap_or_default()),
        };
        values.insert(field.key, value);
    }
    values
}

/// A Secret's `data` entry as text a row can hold: UTF-8 without line breaks
/// or NULs. Anything else is left out of the form rather than damaged by it.
fn row_text(encoded: &str) -> Option<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    (!text.contains(['\n', '\r', '\0'])).then_some(text)
}

/// The rows a Secret's form shows: what `data` holds that a row can carry, with
/// any `stringData` on top, as it would be applied.
fn secret_rows(object: &Value) -> Vec<(String, String)> {
    let mut rows: BTreeMap<String, String> = BTreeMap::new();
    if let Some(data) = object.get("data").and_then(Value::as_object) {
        for (key, value) in data {
            if let Some(text) = value.as_str().and_then(row_text) {
                rows.insert(key.clone(), text);
            }
        }
    }
    if let Some(plain) = object.get("stringData").and_then(Value::as_object) {
        for (key, value) in plain {
            if let Some(text) = text_of(value) {
                rows.insert(key.clone(), text);
            }
        }
    }
    rows.into_iter().collect()
}

/// Write a Secret's rows into `data`, base64-encoded, and drop `stringData`,
/// which the rows now cover. Entries the rows cannot show are kept as they were,
/// unless a row of the same name replaces them.
fn write_secret_rows(rows: &[(String, String)], object: &mut Value) {
    use base64::Engine as _;
    let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);

    let mut data = Map::new();
    if let Some(existing) = object.get("data").and_then(Value::as_object) {
        for (key, value) in existing {
            if value.as_str().and_then(row_text).is_none() {
                data.insert(key.clone(), value.clone());
            }
        }
    }
    for (key, value) in rows.iter().filter(|(key, _)| !key.trim().is_empty()) {
        data.insert(key.trim().to_string(), Value::String(encode(value)));
    }
    remove_path(object, "stringData");
    if data.is_empty() {
        remove_path(object, "data");
    } else {
        set_path(object, "data", Value::Object(data));
    }
}

/// Fields that identify an object, which cannot be changed once it exists.
const IMMUTABLE: &[&str] = &["name", "namespace"];

/// Write a form's values back into an existing object, touching only the
/// fields the layout covers. A field emptied is removed, so the cluster's
/// default applies again, and everything the layout does not know about,
/// including the selector a Deployment cannot change, is left exactly as it was.
pub fn apply_values(spec: &FormSpec, values: &Values, object: &mut Value) {
    for field in spec
        .fields()
        .filter(|field| !IMMUTABLE.contains(&field.key))
    {
        if field.kind == Kind::Secret {
            if let Some(FieldValue::Pairs(rows)) = values.get(field.key) {
                write_secret_rows(rows, object);
            }
            continue;
        }
        let desired = values
            .get(field.key)
            .and_then(|value| field_json(field, value));
        let path = match (field.kind, &desired) {
            (Kind::Lookup { numeric: true, .. }, Some(Value::String(_))) => field
                .path
                .strip_suffix("number")
                .map_or_else(|| field.path.to_string(), |stem| format!("{stem}name")),
            _ => field.path.to_string(),
        };
        match desired {
            Some(Value::Array(mut written)) if matches!(field.kind, Kind::Pairs { list: true }) => {
                // Keep the entries that read from a Secret or ConfigMap: the
                // form only shows plain values, and must not drop the rest.
                if let Some(Value::Array(existing)) = get_path(object, &path) {
                    written.extend(
                        existing
                            .iter()
                            .filter(|e| e.get("value").is_none())
                            .cloned(),
                    );
                }
                set_path(object, &path, Value::Array(written));
            }
            Some(value) => {
                // A port that changed between a number and a name leaves the
                // other form of it behind otherwise.
                if let Kind::Lookup { numeric: true, .. } = field.kind
                    && let Some(stem) = field.path.strip_suffix("number")
                {
                    remove_path(object, &format!("{stem}number"));
                    remove_path(object, &format!("{stem}name"));
                }
                set_path(object, &path, value);
            }
            None => {
                remove_path(object, field.path);
                if let Kind::Lookup { numeric: true, .. } = field.kind
                    && let Some(stem) = field.path.strip_suffix("number")
                {
                    remove_path(object, &format!("{stem}name"));
                }
            }
        }
    }
}

pub fn to_yaml(manifest: &Value) -> Result<String, String> {
    serde_yaml_ng::to_string(manifest).map_err(|err| err.to_string())
}

/// Names Kubernetes accepts for most objects: lowercase letters, digits, `-`
/// and `.`, beginning and ending with a letter or digit.
pub fn check_name(name: &str) -> Result<(), &'static str> {
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if name.is_empty() {
        return Err("Give it a name.");
    }
    if name.len() > 253 {
        return Err("A name can be at most 253 characters.");
    }
    if !name.chars().all(|c| edge(c) || matches!(c, '-' | '.')) {
        return Err("A name uses lowercase letters, digits, - and . only.");
    }
    if !name.starts_with(edge) || !name.ends_with(edge) {
        return Err("A name must start and end with a letter or digit.");
    }
    Ok(())
}

/// What is wrong with a form, by field key, so each can be said next to its field.
pub fn problems(spec: &FormSpec, values: &Values) -> Vec<(&'static str, String)> {
    let mut found = Vec::new();
    for field in spec.fields() {
        let value = values.get(field.key);
        let blank = value.is_none_or(FieldValue::is_empty);
        if blank {
            if field.required {
                found.push((field.key, "Required.".to_string()));
            }
            continue;
        }
        let Some(FieldValue::Text(text)) = value else {
            continue;
        };
        let text = text.trim();
        match field.kind {
            Kind::Number if text.parse::<i64>().is_err() => {
                found.push((field.key, "Enter a whole number.".to_string()));
            }
            Kind::Text if field.key == "name" => {
                if let Err(reason) = check_name(text) {
                    found.push((field.key, reason.to_string()));
                }
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{FieldValue, SPECS, Values, build, check_name, problems, set_path, to_yaml};
    use serde_json::json;

    fn spec(kind: &str) -> &'static super::FormSpec {
        SPECS
            .iter()
            .find(|spec| spec.kind == kind)
            .expect("a layout")
    }

    fn text(value: &str) -> FieldValue {
        FieldValue::Text(value.to_string())
    }

    #[test]
    fn a_dotted_path_makes_objects_and_list_elements_on_the_way() {
        let mut value = json!({});
        set_path(&mut value, "spec.ports.0.port", json!(80));
        set_path(&mut value, "spec.ports.0.name", json!("http"));
        assert_eq!(
            value,
            json!({"spec": {"ports": [{"port": 80, "name": "http"}]}})
        );
    }

    #[test]
    fn a_deployment_gets_a_selector_and_pod_labels_that_agree_with_its_name() {
        let mut values = Values::new();
        values.insert("name", text("web"));
        values.insert("namespace", text("shop"));
        values.insert("image", text("nginx:1.27"));
        values.insert("replicas", text("3"));
        values.insert(
            "env",
            FieldValue::Pairs(vec![("MODE".into(), "prod".into())]),
        );
        let manifest = build(spec("Deployment"), &values);
        assert_eq!(manifest["spec"]["replicas"], json!(3));
        assert_eq!(
            manifest["spec"]["selector"]["matchLabels"]["app"],
            json!("web")
        );
        assert_eq!(
            manifest["spec"]["template"]["metadata"]["labels"]["app"],
            json!("web")
        );
        assert_eq!(
            manifest["spec"]["template"]["spec"]["containers"][0]["env"],
            json!([{"name": "MODE", "value": "prod"}])
        );
    }

    #[test]
    fn a_blank_field_is_left_out_so_the_clusters_default_applies() {
        let mut values = Values::new();
        values.insert("name", text("web"));
        values.insert("image", text("nginx"));
        values.insert("cpu_request", text("  "));
        let manifest = build(spec("Deployment"), &values);
        assert!(
            manifest
                .pointer("/spec/template/spec/containers/0/resources")
                .is_none()
        );
    }

    #[test]
    fn a_target_port_is_a_number_or_a_name_and_a_backend_port_is_a_number() {
        let mut values = Values::new();
        values.insert("name", text("web"));
        values.insert("port", text("80"));
        values.insert("target_port", text("http"));
        let service = build(spec("Service"), &values);
        assert_eq!(service["spec"]["ports"][0]["targetPort"], json!("http"));
        values.insert("target_port", text("8080"));
        assert_eq!(
            build(spec("Service"), &values)["spec"]["ports"][0]["targetPort"],
            json!(8080)
        );

        let mut ingress = Values::new();
        ingress.insert("name", text("site"));
        ingress.insert("service", text("web"));
        ingress.insert("service_port", text("80"));
        let manifest = build(spec("Ingress"), &ingress);
        let backend = &manifest["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"];
        assert_eq!(backend["port"]["number"], json!(80));

        // A named service port is offered by name, and goes to `name`.
        ingress.insert("service_port", text("http"));
        let manifest = build(spec("Ingress"), &ingress);
        let port = &manifest["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"]["port"];
        assert_eq!(port["name"], json!("http"));
        assert!(port.get("number").is_none());
    }

    #[test]
    fn a_command_is_split_into_words() {
        let mut values = Values::new();
        values.insert("command", text("sh -c  echo"));
        let manifest = build(spec("CronJob"), &values);
        assert_eq!(
            manifest["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["command"],
            json!(["sh", "-c", "echo"])
        );
    }

    #[test]
    fn what_a_form_needs_is_said_next_to_the_field_that_lacks_it() {
        let mut values = Values::new();
        values.insert("replicas", text("many"));
        let found = problems(spec("Deployment"), &values);
        let keys: Vec<&str> = found.iter().map(|(key, _)| *key).collect();
        assert!(keys.contains(&"name") && keys.contains(&"namespace") && keys.contains(&"image"));
        assert!(keys.contains(&"replicas"), "a bad number is a problem too");

        values.insert("name", text("Web App"));
        let found = problems(spec("Deployment"), &values);
        assert!(
            found
                .iter()
                .any(|(key, reason)| *key == "name" && reason.contains("lowercase"))
        );
    }

    #[test]
    fn names_follow_kubernetes_rules() {
        for ok in ["web", "my-app", "app.v2", "9lives"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Web", "my_app", "-app", "app-"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_layout_builds_a_manifest_that_names_its_kind_and_serialises() {
        for spec in SPECS {
            let mut values = Values::new();
            for field in spec.fields() {
                if !field.default.is_empty() {
                    values.insert(field.key, text(field.default));
                }
            }
            let manifest = build(spec, &values);
            assert_eq!(manifest["kind"], json!(spec.kind));
            assert!(to_yaml(&manifest).is_ok(), "{}", spec.kind);
        }
    }

    #[test]
    fn what_a_form_reads_from_an_object_it_writes_back_unchanged() {
        let object = json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "namespace": "shop", "labels": {"app": "web"}, "uid": "u1"},
            "spec": {
                "replicas": 3,
                "selector": {"matchLabels": {"app": "web"}},
                "template": {"spec": {"containers": [{
                    "name": "app", "image": "nginx:1.27",
                    "env": [{"name": "MODE", "value": "prod"}],
                    "resources": {"requests": {"cpu": "100m"}}
                }]}}
            }
        });
        let values = super::read_values(spec("Deployment"), &object);
        assert_eq!(values.get("replicas"), Some(&text("3")));
        assert_eq!(values.get("image"), Some(&text("nginx:1.27")));
        let mut rewritten = object.clone();
        super::apply_values(spec("Deployment"), &values, &mut rewritten);
        assert_eq!(rewritten, object, "reading then writing changes nothing");
    }

    #[test]
    fn an_edit_changes_its_field_and_leaves_everything_else_alone() {
        let object = json!({
            "kind": "Deployment",
            "metadata": {"name": "web", "uid": "u1"},
            "spec": {"replicas": 1, "paused": true, "template": {"spec": {"containers": [{"name": "app", "image": "a"}]}}}
        });
        let mut values = super::read_values(spec("Deployment"), &object);
        values.insert("replicas", text("5"));
        values.insert("image", text("b"));
        values.insert("name", text("renamed"));
        let mut edited = object.clone();
        super::apply_values(spec("Deployment"), &values, &mut edited);
        assert_eq!(edited["spec"]["replicas"], json!(5));
        assert_eq!(
            edited["spec"]["template"]["spec"]["containers"][0]["image"],
            json!("b")
        );
        assert_eq!(
            edited["spec"]["paused"],
            json!(true),
            "a field the layout does not cover"
        );
        assert_eq!(
            edited["metadata"]["name"],
            json!("web"),
            "a name cannot be changed"
        );
        assert!(
            edited["spec"].get("selector").is_none(),
            "no selector is invented on an edit"
        );
    }

    #[test]
    fn an_emptied_field_is_removed_with_the_objects_it_leaves_empty() {
        let object = json!({
            "kind": "Deployment",
            "metadata": {"name": "web"},
            "spec": {"template": {"spec": {"containers": [{
                "name": "app", "image": "a", "resources": {"requests": {"cpu": "100m"}}
            }]}}}
        });
        let mut values = super::read_values(spec("Deployment"), &object);
        values.insert("cpu_request", text(""));
        let mut edited = object.clone();
        super::apply_values(spec("Deployment"), &values, &mut edited);
        assert!(
            edited
                .pointer("/spec/template/spec/containers/0/resources")
                .is_none()
        );
        assert_eq!(
            edited["spec"]["template"]["spec"]["containers"][0]["image"],
            json!("a")
        );
    }

    fn secret() -> serde_json::Value {
        // user=admin, pass=s3cret, a multi-line certificate and a binary blob.
        json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "db"},
            "type": "Opaque",
            "data": {
                "user": "YWRtaW4=",
                "pass": "czNjcmV0",
                "tls.crt": "LS0tLS1CRUdJTgphYmMKLS0tLS1FTkQ=",
                "blob": "/wCA"
            }
        })
    }

    #[test]
    fn an_existing_secret_shows_its_decoded_values_but_not_what_a_row_cannot_hold() {
        let values = super::read_values(spec("Secret"), &secret());
        assert_eq!(
            values.get("string_data"),
            Some(&FieldValue::Pairs(vec![
                ("pass".into(), "s3cret".into()),
                ("user".into(), "admin".into()),
            ])),
            "the certificate has line breaks and the blob is binary"
        );
    }

    #[test]
    fn editing_a_secret_writes_data_keeps_the_hidden_entries_and_drops_string_data() {
        let object = secret();
        let mut values = super::read_values(spec("Secret"), &object);
        values.insert(
            "string_data",
            FieldValue::Pairs(vec![
                ("user".into(), "root".into()),
                ("token".into(), "abc".into()),
            ]),
        );
        let mut edited = object.clone();
        edited["stringData"] = json!({"old": "x"});
        super::apply_values(spec("Secret"), &values, &mut edited);

        assert!(edited.get("stringData").is_none());
        let data = &edited["data"];
        assert_eq!(data["user"], json!("cm9vdA=="), "changed");
        assert_eq!(data["token"], json!("YWJj"), "added");
        assert!(data.get("pass").is_none(), "removed in the form");
        assert_eq!(data["tls.crt"], object["data"]["tls.crt"], "kept as it was");
        assert_eq!(data["blob"], object["data"]["blob"], "kept as it was");
    }

    #[test]
    fn a_secret_whose_rows_are_all_cleared_loses_data_and_a_new_one_still_uses_string_data() {
        let object = json!({
            "kind": "Secret", "metadata": {"name": "s"}, "data": {"a": "Yg=="}
        });
        let mut values = super::read_values(spec("Secret"), &object);
        values.insert("string_data", FieldValue::Pairs(Vec::new()));
        let mut edited = object.clone();
        super::apply_values(spec("Secret"), &values, &mut edited);
        assert!(edited.get("data").is_none());

        let mut created = super::Values::new();
        created.insert("name", FieldValue::Text("s".into()));
        created.insert(
            "string_data",
            FieldValue::Pairs(vec![("k".into(), "v".into())]),
        );
        let built = super::build(spec("Secret"), &created);
        assert_eq!(built["stringData"]["k"], json!("v"));
        assert!(built.get("data").is_none());
    }

    #[test]
    fn an_environment_value_from_a_secret_survives_an_edit_of_the_plain_ones() {
        let object = json!({
            "kind": "Deployment",
            "metadata": {"name": "web"},
            "spec": {"template": {"spec": {"containers": [{
                "name": "app", "image": "a",
                "env": [
                    {"name": "PLAIN", "value": "1"},
                    {"name": "TOKEN", "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}}
                ]
            }]}}}
        });
        let mut values = super::read_values(spec("Deployment"), &object);
        assert_eq!(
            values.get("env"),
            Some(&FieldValue::Pairs(vec![("PLAIN".into(), "1".into())])),
            "only the plain value is shown"
        );
        values.insert("env", FieldValue::Pairs(vec![("PLAIN".into(), "2".into())]));
        let mut edited = object.clone();
        super::apply_values(spec("Deployment"), &values, &mut edited);
        let env = &edited["spec"]["template"]["spec"]["containers"][0]["env"];
        assert_eq!(env[0], json!({"name": "PLAIN", "value": "2"}));
        assert_eq!(env[1]["name"], json!("TOKEN"));
        assert!(
            env[1].get("valueFrom").is_some(),
            "the secret reference is kept"
        );
    }

    #[test]
    fn a_backend_port_read_by_name_is_written_back_by_name() {
        let object = json!({"kind": "Ingress", "metadata": {"name": "s"}, "spec": {"rules": [{"http": {"paths": [{
            "backend": {"service": {"name": "web", "port": {"name": "http"}}}
        }]}}]}});
        let mut values = super::read_values(spec("Ingress"), &object);
        assert_eq!(values.get("service_port"), Some(&text("http")));
        values.insert("service_port", text("8080"));
        let mut edited = object.clone();
        super::apply_values(spec("Ingress"), &values, &mut edited);
        let port = &edited["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"]["port"];
        assert_eq!(
            port,
            &json!({"number": 8080}),
            "the name is gone when a number is chosen"
        );
    }
}
