//! Finding a newer release, and installing it only if it is genuinely ours.
//!
//! The app is not signed with a Developer ID, so the operating system cannot
//! vouch for an update, and this app holds credentials for production
//! clusters. A downloaded program that replaces it must therefore prove where it
//! came from. Releases carry an Ed25519 signature over the package's SHA-256, made
//! with a key only the release workflow holds; the matching public key is
//! compiled in. An update that does not verify is deleted, never installed.
//!
//! If no public key is set ([`PUBLIC_KEY_HEX`] empty) nothing can be installed,
//! and the app only tells the person a release exists and where to get it.
//!
//! Checking for a release asks GitHub one question and sends nothing about this
//! machine. Installing is always an explicit click.
//!
//! Looking for a release and downloading it lean on `curl`, which ships with
//! macOS, Windows 10 and later, and nearly every Linux. Unpacking leans on `ditto`
//! (macOS) or `tar` (Linux, and Windows 10 and later), which avoids an HTTP
//! client and an archive reader for two calls. What replaces the running copy
//! is in [`crate::update_install`].

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub use crate::update_install::{current_install, install, stage};

use ring::{
    digest,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde_json::Value;

/// Where releases are published.
pub const REPO: &str = "terdiam/kubernaut";

/// Hex of the Ed25519 public key releases are signed with. The matching private
/// key is the repository secret `UPDATE_SIGNING_KEY`; see
/// `examples/update_tool.rs`. Replacing this key stops every installed copy from
/// accepting updates until it is reinstalled by hand.
pub const PUBLIC_KEY_HEX: &str = "0def8acb94b255bf40ec475d5f38a96b257c41eb64cdda8c1d9b7571cf9b15c3";

/// The most that is downloaded for one release.
const MAX_DOWNLOAD: &str = "200M";

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release's page, with its notes.
    pub page_url: String,
    /// The package for this machine, if the release has one.
    pub package_url: Option<String>,
    /// Its signature, if the release has one.
    pub sig_url: Option<String>,
}

impl Release {
    /// Whether this build can install it: a key is compiled in and the release
    /// is signed.
    pub fn installable(&self) -> bool {
        cfg!(any(target_os = "macos", target_os = "linux", windows))
            && public_key().is_some()
            && self.package_url.is_some()
            && self.sig_url.is_some()
    }
}

/// The compiled-in public key, if one is set and well formed.
pub fn compiled_public_key() -> Option<Vec<u8>> {
    public_key()
}

fn public_key() -> Option<Vec<u8>> {
    let bytes = hex_decode(PUBLIC_KEY_HEX)?;
    (bytes.len() == 32).then_some(bytes)
}

/// The file a release publishes for one operating system and architecture, as
/// the bundle scripts name it. `os` and `arch` are `std::env::consts` values.
/// macOS spells Apple silicon `arm64`; Linux keeps `uname -m`'s `aarch64`.
fn package_name(version: &str, os: &str, arch: &str) -> String {
    match os {
        "macos" => {
            let arch = if arch == "aarch64" { "arm64" } else { arch };
            format!("Kubernaut-{version}-macos-{arch}.zip")
        }
        "windows" => format!("Kubernaut-{version}-windows-{arch}.zip"),
        _ => format!("Kubernaut-{version}-{os}-{arch}.tar.gz"),
    }
}

/// `0.1.15` as numbers. A pre-release or build suffix makes it unparseable on
/// purpose: such a version is never offered.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Whether `candidate` is a newer plain version than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

/// Read GitHub's "latest release" document.
pub fn parse_release(json: &str, os: &str, arch: &str) -> Result<Release, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable release: {e}"))?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("the release has no version")?;
    let version = tag.trim_start_matches('v').to_string();
    let page_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let package = package_name(&version, os, arch);
    let asset = |name: &str| {
        value
            .get("assets")
            .and_then(Value::as_array)?
            .iter()
            .find(|asset| asset.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|asset| asset.get("browser_download_url")?.as_str())
            .map(str::to_string)
    };
    Ok(Release {
        sig_url: asset(&format!("{package}.sig")),
        package_url: asset(&package),
        version,
        page_url,
    })
}

/// A debug build can be pointed at a local file instead of GitHub, to try the
/// whole path offline. A release build ignores this.
fn url_override() -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var("KUBERNAUT_UPDATE_URL").ok()
    } else {
        None
    }
}

/// Run `curl` with fixed, conservative options.
fn curl(url: &str, output: Option<&Path>) -> Result<Vec<u8>, String> {
    // https only; the override of a debug build may also name a file.
    let protocols = if url_override().is_some() {
        "=https,file"
    } else {
        "=https"
    };
    // macOS ships curl at a fixed path; elsewhere it is found on PATH
    // (Windows 10 and later include curl.exe).
    let curl = if cfg!(target_os = "macos") {
        "/usr/bin/curl"
    } else {
        "curl"
    };
    let mut command = Command::new(curl);
    command
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--proto", protocols, "--proto-redir", "=https"])
        .args(["--max-time", "300", "--max-filesize", MAX_DOWNLOAD])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", "User-Agent: Kubernaut"]);
    if let Some(path) = output {
        command.arg("--output").arg(path);
    }
    let result = command
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if result.status.success() {
        Ok(result.stdout)
    } else {
        Err(String::from_utf8_lossy(&result.stderr).trim().to_string())
    }
}

/// Ask for the latest release; `None` when the running version is current.
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let url = url_override()
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let body = curl(&url, None)?;
    let release = parse_release(
        &String::from_utf8_lossy(&body),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    Ok(is_newer(current, &release.version).then_some(release))
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// SHA-256 of a file as hex, read in pieces so a large zip is not held in memory.
fn file_digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..read) {
            context.update(chunk);
        }
    }
    Ok(hex_encode(context.finish().as_ref()))
}

/// Whether `signature_hex` is `key`'s signature of the file's SHA-256 (as hex text).
pub fn verify(key: &[u8], zip: &Path, signature_hex: &str) -> Result<(), String> {
    let signature = hex_decode(signature_hex).ok_or("the signature is not hex")?;
    let message = file_digest(zip)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(message.as_bytes(), &signature)
        .map_err(|_| "the signature does not match this download".to_string())
}

/// A new key pair as `(private key hex, public key hex)`. For the release tool.
pub fn generate_keypair() -> Result<(String, String), String> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| "could not generate a key".to_string())?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| "the generated key is not usable".to_string())?;
    Ok((
        hex_encode(pkcs8.as_ref()),
        hex_encode(pair.public_key().as_ref()),
    ))
}

/// Sign a zip with a private key from [`generate_keypair`]; the signature, as hex.
pub fn sign(private_key_hex: &str, zip: &Path) -> Result<String, String> {
    let pkcs8 = hex_decode(private_key_hex).ok_or("the private key is not hex")?;
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| "the private key is not valid")?;
    let message = file_digest(zip)?;
    Ok(hex_encode(pair.sign(message.as_bytes()).as_ref()))
}

/// Download a release's package and signature into `directory` and verify them.
/// Returns the verified package; on any failure nothing is left to install.
pub fn download(release: &Release, directory: &Path) -> Result<PathBuf, String> {
    let key = public_key().ok_or("this build has no update key, so it cannot install updates")?;
    let (Some(package_url), Some(sig_url)) = (&release.package_url, &release.sig_url) else {
        return Err("this release has no signed download for this machine".into());
    };
    let package = directory.join("update-package");
    curl(package_url, Some(&package))?;
    let signature = curl(sig_url, None)?;
    let checked = verify(&key, &package, &String::from_utf8_lossy(&signature));
    if let Err(reason) = checked {
        let _ = std::fs::remove_file(&package);
        return Err(reason);
    }
    Ok(package)
}

/// A fresh folder to work in, which the swap helper removes.
pub fn staging_directory() -> Result<PathBuf, String> {
    tempfile::Builder::new()
        .prefix("kubernaut-update-")
        .tempdir()
        .map(tempfile::TempDir::keep)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn only_a_plain_higher_version_is_newer() {
        assert!(is_newer("0.1.15", "0.1.16"));
        assert!(is_newer("0.1.15", "v0.2.0"));
        assert!(is_newer("0.9.9", "0.10.0"), "numbers, not text");
        assert!(!is_newer("0.1.15", "0.1.15"));
        assert!(!is_newer("0.1.15", "0.1.9"));
        assert!(
            !is_newer("0.1.15", "0.2.0-rc.1"),
            "a pre-release is never offered"
        );
        assert!(!is_newer("0.1.15", "latest"));
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.1.16",
        "html_url": "https://github.com/terdiam/kubernaut/releases/tag/v0.1.16",
        "assets": [
            {"name": "Kubernaut-0.1.16-macos-arm64.zip", "browser_download_url": "https://x/zip"},
            {"name": "Kubernaut-0.1.16-macos-arm64.zip.sig", "browser_download_url": "https://x/sig"},
            {"name": "Kubernaut-0.1.16-macos-x86_64.zip", "browser_download_url": "https://x/intel"},
            {"name": "Kubernaut-0.1.16-linux-aarch64.tar.gz", "browser_download_url": "https://x/linux"},
            {"name": "Kubernaut-0.1.16-linux-aarch64.tar.gz.sig", "browser_download_url": "https://x/linux-sig"},
            {"name": "Kubernaut-0.1.16-windows-x86_64.zip", "browser_download_url": "https://x/win"},
            {"name": "Kubernaut-0.1.16-windows-x86_64-setup.exe", "browser_download_url": "https://x/setup"}
        ]
    }"#;

    #[test]
    fn a_release_is_read_with_the_assets_for_this_machine() {
        let release = parse_release(RELEASE, "macos", "aarch64").unwrap();
        assert_eq!(release.version, "0.1.16");
        assert_eq!(release.package_url.as_deref(), Some("https://x/zip"));
        assert_eq!(release.sig_url.as_deref(), Some("https://x/sig"));

        let intel = parse_release(RELEASE, "macos", "x86_64").unwrap();
        assert_eq!(intel.package_url.as_deref(), Some("https://x/intel"));
        assert!(intel.sig_url.is_none(), "unsigned for this machine");

        let linux = parse_release(RELEASE, "linux", "aarch64").unwrap();
        assert_eq!(linux.package_url.as_deref(), Some("https://x/linux"));
        assert_eq!(linux.sig_url.as_deref(), Some("https://x/linux-sig"));

        let windows = parse_release(RELEASE, "windows", "x86_64").unwrap();
        assert_eq!(windows.package_url.as_deref(), Some("https://x/win"));
        assert!(
            windows.sig_url.is_none(),
            "the installer is not the package"
        );

        assert!(parse_release("{}", "macos", "aarch64").is_err());
        assert!(parse_release("not json", "macos", "aarch64").is_err());
    }

    #[test]
    fn each_system_asks_for_its_own_file() {
        assert_eq!(
            package_name("1.2.3", "macos", "aarch64"),
            "Kubernaut-1.2.3-macos-arm64.zip"
        );
        assert_eq!(
            package_name("1.2.3", "macos", "x86_64"),
            "Kubernaut-1.2.3-macos-x86_64.zip"
        );
        assert_eq!(
            package_name("1.2.3", "linux", "aarch64"),
            "Kubernaut-1.2.3-linux-aarch64.tar.gz"
        );
        assert_eq!(
            package_name("1.2.3", "linux", "x86_64"),
            "Kubernaut-1.2.3-linux-x86_64.tar.gz"
        );
        assert_eq!(
            package_name("1.2.3", "windows", "x86_64"),
            "Kubernaut-1.2.3-windows-x86_64.zip"
        );
    }

    fn zip_file(contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update.zip");
        File::create(&path).unwrap().write_all(contents).unwrap();
        (directory, path)
    }

    #[test]
    fn a_signed_zip_verifies_and_anything_else_does_not() {
        let (private, public) = generate_keypair().unwrap();
        let key = hex_decode(&public).unwrap();
        let (_dir, zip) = zip_file(b"the real release");
        let signature = sign(&private, &zip).unwrap();
        assert!(verify(&key, &zip, &signature).is_ok());

        // Tampered contents.
        let (_other, tampered) = zip_file(b"the real release, plus something");
        assert!(verify(&key, &tampered, &signature).is_err());

        // Signed by somebody else.
        let (stranger, _) = generate_keypair().unwrap();
        let forged = sign(&stranger, &zip).unwrap();
        assert!(verify(&key, &zip, &forged).is_err());

        // Not a signature at all.
        assert!(verify(&key, &zip, "zz").is_err());
        assert!(verify(&key, &zip, "").is_err());
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        assert_eq!(hex_decode("00ff10").unwrap(), [0, 255, 16]);
        assert_eq!(hex_encode(&[0, 255, 16]), "00ff10");
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("gg").is_none());
    }

    #[test]
    fn nothing_is_installable_while_no_key_is_compiled_in() {
        let release = parse_release(RELEASE, "macos", "aarch64").unwrap();
        assert_eq!(release.installable(), public_key().is_some());
    }
}
