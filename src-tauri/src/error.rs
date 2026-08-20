use serde::{Serialize, Serializer};

/// Error type every command returns.
///
/// Commands must not leak `kube::Error` verbatim: it can embed request URLs
/// containing tokens in some auth paths. `k8s-core` errors are already written
/// as user-facing sentences, so this is a thin string wrapper.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CommandError(String);

impl Serialize for CommandError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl From<k8s_core::CoreError> for CommandError {
    fn from(err: k8s_core::CoreError) -> Self {
        Self(err.to_string())
    }
}

impl From<tauri::Error> for CommandError {
    fn from(err: tauri::Error) -> Self {
        Self(err.to_string())
    }
}

impl CommandError {
    pub fn new(msg: impl std::fmt::Display) -> Self {
        Self(msg.to_string())
    }
}

pub type CommandResult<T> = std::result::Result<T, CommandError>;
