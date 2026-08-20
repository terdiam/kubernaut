use std::fmt;

/// Errors surfaced by `k8s-core`.
///
/// Variants are deliberately coarse: the UI shows `to_string()` directly, so
/// each message must be actionable on its own.
/// `kube::Error` is several hundred bytes, so every variant carrying one is
/// boxed — otherwise every `Result` in the crate pays for the error path.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("kubeconfig error: {0}")]
    Kubeconfig(Box<kube::config::KubeconfigError>),

    #[error("failed to build client for context `{context}`: {source}")]
    ClientBuild {
        context: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("kubernetes api error: {0}")]
    Api(Box<kube::Error>),

    #[error("discovery failed for cluster `{cluster}`: {source}")]
    Discovery {
        cluster: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("unknown cluster `{0}` (not connected)")]
    UnknownCluster(String),

    #[error("context `{0}` not found in kubeconfig")]
    UnknownContext(String),

    #[error("resource `{0}` not found in cluster discovery")]
    UnknownResource(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl From<kube::Error> for CoreError {
    fn from(err: kube::Error) -> Self {
        Self::Api(Box::new(err))
    }
}

impl From<kube::config::KubeconfigError> for CoreError {
    fn from(err: kube::config::KubeconfigError) -> Self {
        Self::Kubeconfig(Box::new(err))
    }
}

impl CoreError {
    pub fn other(msg: impl fmt::Display) -> Self {
        Self::Other(msg.to_string())
    }

    pub fn client_build(context: &str, source: kube::Error) -> Self {
        // A rejected credential is the single most common connection failure and
        // the raw "Unauthorized" tells the user nothing about what to do, so
        // turn the two auth codes into an instruction instead.
        if let kube::Error::Api(status) = &source {
            let hint = match status.code {
                401 => Some(
                    "credentials were rejected. The token or client certificate for this \
                     context has most likely expired — re-authenticate (for example \
                     `aws eks update-kubeconfig`, `gcloud container clusters get-credentials`, \
                     `az aks get-credentials`, or your provider's login command) and reload \
                     the kubeconfig",
                ),
                403 => Some(
                    "the credentials are valid but this user may not access the cluster \
                     endpoint. Check the RBAC bindings for this account, or pick a different \
                     context",
                ),
                _ => None,
            };
            if let Some(hint) = hint {
                return Self::Other(format!("cannot connect to `{context}`: {hint}"));
            }
        }
        Self::ClientBuild {
            context: context.to_string(),
            source: Box::new(source),
        }
    }

    pub fn discovery(cluster: &str, source: kube::Error) -> Self {
        Self::Discovery {
            cluster: cluster.to_string(),
            source: Box::new(source),
        }
    }
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
