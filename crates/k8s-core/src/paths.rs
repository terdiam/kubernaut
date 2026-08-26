//! PATH recovery for GUI launches.
//!
//! A desktop app started from Finder/Dock (macOS) or a `.desktop` entry (Linux)
//! inherits a minimal `PATH` that does not include Homebrew, asdf, nvm, or the
//! cloud CLIs. Every kubeconfig that authenticates through an `exec` credential
//! plugin (`aws`, `gke-gcloud-auth-plugin`, `az`, `kubelogin`) then fails with a
//! confusing "no such file or directory" — the single most common cause of
//! "cluster won't connect" reports in tools of this kind.
//!
//! We recover the real PATH once at startup by asking the user's login shell.

use std::{collections::HashSet, ffi::OsString, path::PathBuf};

/// Generous, because a prompt framework (powerlevel10k, oh-my-zsh, starship)
/// plus completion rebuilding can take several seconds on a cold cache. Timing
/// out here is not fatal — the fallback directories still apply — but it costs
/// the user their real PATH, which is the whole point of this module.
/// Windows has no login-shell step to time, and `-D warnings` in CI turns an
/// unused constant into a build failure there.
#[cfg(not(windows))]
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Resolve the login shell `PATH` and merge it into this process' environment.
///
/// Returns the effective `PATH` entries after the merge. Idempotent, and a
/// no-op on Windows (where GUI processes inherit the user PATH already).
pub async fn hydrate_process_path(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let push = |p: PathBuf, entries: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>| {
        if !p.as_os_str().is_empty() && seen.insert(p.clone()) {
            entries.push(p);
        }
    };

    // User-configured entries win: they are the escape hatch when shell probing
    // cannot work (custom launchers, nix, containerised dev setups).
    for p in extra {
        push(p.clone(), &mut entries, &mut seen);
    }
    if let Some(current) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&current) {
            push(p, &mut entries, &mut seen);
        }
    }
    if let Some(shell_path) = login_shell_path().await {
        for p in std::env::split_paths(&shell_path) {
            push(p, &mut entries, &mut seen);
        }
    }
    for p in fallback_dirs() {
        push(p, &mut entries, &mut seen);
    }

    match std::env::join_paths(&entries) {
        Ok(joined) => unsafe { std::env::set_var("PATH", &joined) },
        Err(err) => tracing::warn!(%err, "could not join PATH entries; leaving PATH untouched"),
    }
    entries
}

#[cfg(windows)]
async fn login_shell_path() -> Option<OsString> {
    None
}

/// Ask the login shell what `PATH` an interactive session would have.
///
/// `-ilc` sources the user's rc files. Some setups print banners, so we take
/// the last non-empty line and require it to look like a PATH.
#[cfg(not(windows))]
async fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let run = tokio::process::Command::new(&shell)
        .args(["-ilc", "printf '<<PATH>>%s<<PATH>>' \"$PATH\""])
        // Prompt frameworks are the main source of startup latency here, and
        // none of their output matters to us. These are the documented opt-outs
        // for the common ones.
        .env("DISABLE_AUTO_UPDATE", "true")
        .env("DISABLE_UPDATE_PROMPT", "true")
        .env("POWERLEVEL9K_INSTANT_PROMPT", "off")
        .env("POWERLEVEL9K_DISABLE_GITSTATUS", "true")
        .env("ZSH_DISABLE_COMPFIX", "true")
        .env("STARSHIP_LOG", "error")
        .stdin(std::process::Stdio::null())
        .output();

    let out = match tokio::time::timeout(SHELL_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(out)) => {
            tracing::warn!(shell = %shell, status = ?out.status, "login shell exited non-zero");
            return None;
        }
        Ok(Err(err)) => {
            tracing::warn!(shell = %shell, %err, "could not run login shell");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                shell = %shell,
                seconds = SHELL_TIMEOUT.as_secs(),
                "login shell timed out while resolving PATH; falling back to well-known \
                 directories. Cluster contexts using an exec auth plugin may not connect — \
                 add its directory under Settings if so."
            );
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    // Sentinels isolate the value from rc-file banners and MOTD noise.
    let value = stdout.split("<<PATH>>").nth(1)?.trim();
    if value.is_empty() {
        return None;
    }
    Some(OsString::from(value))
}

/// Locations that hold cluster auth plugins on a default install but are
/// missing from a bare GUI `PATH`.
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    #[cfg(target_os = "linux")]
    {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/snap/bin"));
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/bin"));
    }
    if let Some(home) = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()) {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("bin"));
        dirs.push(home.join("google-cloud-sdk/bin"));
    }
    dirs.into_iter().filter(|p| p.is_dir()).collect()
}

/// Look up an executable on the current `PATH`. Used to report which auth
/// plugins are available before a connection is attempted.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
