/// Errors from cluster operations. `k8s-core` errors pass through unchanged so
/// the UI keeps one error vocabulary.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error(transparent)]
    Core(#[from] k8s_core::CoreError),

    #[error("kubernetes api error: {0}")]
    Api(Box<kube::Error>),

    #[error("`{kind}` has no pod selector, so its logs cannot be followed")]
    NoSelector { kind: String },

    #[error("pod `{pod}` has no container named `{container}`")]
    UnknownContainer { pod: String, container: String },

    #[error("no session `{0}` (it may have already been closed)")]
    UnknownSession(String),

    #[error("port {0} is not exposed by this target")]
    UnknownPort(u16),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid yaml: {0}")]
    Yaml(String),

    #[error("{0}")]
    Other(String),
}

impl From<kube::Error> for OpsError {
    fn from(err: kube::Error) -> Self {
        Self::Api(Box::new(err))
    }
}

impl OpsError {
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Self::Other(msg.to_string())
    }
}

pub type Result<T, E = OpsError> = std::result::Result<T, E>;
