//! Finding a newer release, and installing it only if it is genuinely ours.
//!
//! The app is not signed with a Developer ID, so the operating system cannot
//! vouch for an update, and this app holds credentials for production
//! clusters. A downloaded program that replaces it must therefore prove where it
//! came from. Releases carry an Ed25519 signature over the zip's SHA-256, made
//! with a key only the release workflow holds; the matching public key is
//! compiled in. An update that does not verify is deleted, never installed.
//!
//! Until a public key is set ([`PUBLIC_KEY_HEX`]) nothing can be installed, and
//! the app only tells the person a release exists and where to get it.
//!
//! Checking for a release asks GitHub one question and sends nothing about this
//! machine. Installing is always an explicit click.
//!
//! Looking for a release works wherever `curl` does. Installing is macOS only:
//! it leans on the `curl`, `ditto`, `plutil` and `codesign` that ship with it,
//! which avoids an HTTP client and a zip reader for two calls. Elsewhere the app
//! tells the person a release exists and links its notes.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ring::{
    digest,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde_json::Value;

/// Where releases are published.
pub const REPO: &str = "terdiam/kubernaut";

/// Hex of the Ed25519 public key releases are signed with. Empty until the
/// project sets one up; see `examples/update_tool.rs`.
pub const PUBLIC_KEY_HEX: &str = "";

/// Bundle identifier of a genuine release, checked before one replaces this.
const BUNDLE_ID: &str = "dev.kubernaut.app";
/// The most that is downloaded for one release.
const MAX_DOWNLOAD: &str = "200M";

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release's page, with its notes.
    pub page_url: String,
    /// The zip for this machine, if the release has one.
    pub zip_url: Option<String>,
    /// Its signature, if the release has one.
    pub sig_url: Option<String>,
}

impl Release {
    /// Whether this build can install it: a key is compiled in and the release
    /// is signed.
    pub fn installable(&self) -> bool {
        cfg!(target_os = "macos")
            && public_key().is_some()
            && self.zip_url.is_some()
            && self.sig_url.is_some()
    }
}

/// The compiled-in public key, if one is set and well formed.
fn public_key() -> Option<Vec<u8>> {
    let bytes = hex_decode(PUBLIC_KEY_HEX)?;
    (bytes.len() == 32).then_some(bytes)
}

/// The zip's architecture word, as the release script spells it (`uname -m`).
fn arch_word() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
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
pub fn parse_release(json: &str, arch: &str) -> Result<Release, String> {
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

    let zip_name = format!("Kubernaut-{version}-macos-{arch}.zip");
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
        sig_url: asset(&format!("{zip_name}.sig")),
        zip_url: asset(&zip_name),
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
    let release = parse_release(&String::from_utf8_lossy(&body), arch_word())?;
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

/// Whether `signature_hex` is `key`'s signature of the zip's SHA-256 (as hex text).
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

/// Download a release's zip and signature into `directory` and verify them.
/// Returns the verified zip; on any failure nothing is left to install.
pub fn download(release: &Release, directory: &Path) -> Result<PathBuf, String> {
    let key = public_key().ok_or("this build has no update key, so it cannot install updates")?;
    let (Some(zip_url), Some(sig_url)) = (&release.zip_url, &release.sig_url) else {
        return Err("this release has no signed download for this machine".into());
    };
    let zip = directory.join("update.zip");
    curl(zip_url, Some(&zip))?;
    let signature = curl(sig_url, None)?;
    let checked = verify(&key, &zip, &String::from_utf8_lossy(&signature));
    if let Err(reason) = checked {
        let _ = std::fs::remove_file(&zip);
        return Err(reason);
    }
    Ok(zip)
}

/// Unpack a verified zip and check it is the app it claims to be.
pub fn stage(zip: &Path, directory: &Path, version: &str) -> Result<PathBuf, String> {
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(directory)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("could not run ditto: {e}"))?;
    if !status.success() {
        return Err("the download could not be unpacked".into());
    }
    let app = std::fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or("the download holds no app")?;
    check_bundle(&app, version)?;
    Ok(app)
}

fn plist_value(app: &Path, key: &str) -> Option<String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The bundle is this app, at the version announced, with its executable.
pub fn check_bundle(app: &Path, version: &str) -> Result<(), String> {
    if plist_value(app, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err("the download is not Kubernaut".into());
    }
    if plist_value(app, "CFBundleShortVersionString").as_deref() != Some(version) {
        return Err("the download is not the version that was announced".into());
    }
    let executable = app.join("Contents/MacOS/kubernaut-ui");
    if !executable.is_file() {
        return Err("the download has no executable".into());
    }
    let sound = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if sound {
        Ok(())
    } else {
        Err("the downloaded app's own signature is not intact".into())
    }
}

/// The `.app` this process runs from, if it runs from one.
pub fn current_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?)
}

fn bundle_of(executable: &Path) -> Option<PathBuf> {
    // .../Name.app/Contents/MacOS/exe
    let app = executable.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

/// Swap the staged app in once this process has exited, and open it. The
/// previous app is put back if the swap fails.
const SWAP: &str = r#"
pid="$1"; app="$2"; new="$3"; staging="$4"
n=0
while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 150 ]; do sleep 0.2; n=$((n+1)); done
old="$app.updating"
rm -rf "$old"
if mv "$app" "$old" && mv "$new" "$app"; then
  rm -rf "$old"
else
  [ -e "$old" ] && [ ! -e "$app" ] && mv "$old" "$app"
fi
case "$staging" in */kubernaut-update-*) rm -rf "$staging" ;; esac
open "$app"
"#;

/// Hand the swap to a helper that outlives this process. The caller then quits.
pub fn install(staged: &Path, bundle: &Path) -> Result<(), String> {
    let parent = bundle.parent().ok_or("the app has no folder")?;
    // Writing is the real test of whether the folder may be changed.
    tempfile::Builder::new()
        .prefix(".kubernaut-write-test")
        .tempfile_in(parent)
        .map_err(|_| {
            format!(
                "{} cannot be changed from here; drag the new app over it yourself",
                parent.display()
            )
        })?;
    let staging = staged.parent().ok_or("the staged app has no folder")?;

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(SWAP)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(bundle)
        .arg(staged)
        .arg(staging)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start the installer: {e}"))
}

/// Give the installer its own process group, so it is not taken down with this
/// app.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Installing is not offered off macOS, so there is nothing to detach.
#[cfg(not(unix))]
fn detach(_: &mut Command) {}

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
            {"name": "Kubernaut-0.1.16-macos-x86_64.zip", "browser_download_url": "https://x/intel"}
        ]
    }"#;

    #[test]
    fn a_release_is_read_with_the_assets_for_this_machine() {
        let release = parse_release(RELEASE, "arm64").unwrap();
        assert_eq!(release.version, "0.1.16");
        assert_eq!(release.zip_url.as_deref(), Some("https://x/zip"));
        assert_eq!(release.sig_url.as_deref(), Some("https://x/sig"));

        let intel = parse_release(RELEASE, "x86_64").unwrap();
        assert_eq!(intel.zip_url.as_deref(), Some("https://x/intel"));
        assert!(intel.sig_url.is_none(), "unsigned for this machine");

        assert!(parse_release("{}", "arm64").is_err());
        assert!(parse_release("not json", "arm64").is_err());
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
    fn the_app_bundle_is_found_from_the_executable_path() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Kubernaut.app/Contents/MacOS/kubernaut-ui"
            )),
            Some(PathBuf::from("/Applications/Kubernaut.app"))
        );
        assert_eq!(
            bundle_of(Path::new("/work/target/debug/kubernaut-ui")),
            None
        );
    }

    #[test]
    fn nothing_is_installable_while_no_key_is_compiled_in() {
        let release = parse_release(RELEASE, "arm64").unwrap();
        assert_eq!(
            release.installable(),
            cfg!(target_os = "macos") && public_key().is_some()
        );
    }
}
