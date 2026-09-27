//! `kustomize build`, standalone — independent of the Flux GitOps integration,
//! which only ever reads a `Kustomization`'s status; it never runs kustomize
//! itself. Output is plain YAML text, so it flows through the exact same
//! [`crate::manifest`] plan/apply/diff pipeline as a pasted or imported file.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use crate::error::{OpsError, Result};

/// Long enough for an overlay with remote bases to resolve, short enough that
/// a hung process does not wedge the UI forever.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct Kustomize {
    binary: PathBuf,
}

impl Kustomize {
    /// Find kustomize: the copy shipped with the app first, then the user's own.
    pub fn resolve(sidecar_dir: Option<&Path>) -> Result<Self> {
        let candidate = sidecar_dir.map(|dir| {
            dir.join(if cfg!(windows) {
                "kustomize.exe"
            } else {
                "kustomize"
            })
        });

        if let Some(path) = candidate.filter(|path| path.is_file()) {
            return Ok(Self { binary: path });
        }

        match k8s_core::paths::which("kustomize") {
            Some(path) => Ok(Self { binary: path }),
            None => Err(OpsError::other(
                "no bundled kustomize and none on PATH. Install kustomize, or add its directory in Settings.",
            )),
        }
    }

    /// Build an overlay directory into plain multi-document YAML.
    pub async fn build(&self, overlay_path: &Path) -> Result<String> {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(["build", &overlay_path.display().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = command.spawn()?;
        let output = match tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(OpsError::other(format!(
                    "kustomize build timed out after {}s",
                    COMMAND_TIMEOUT.as_secs()
                )));
            }
        };

        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(OpsError::other(if message.is_empty() {
                format!("kustomize build exited with {}", output.status)
            } else {
                message
            }));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
