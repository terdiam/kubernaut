//! Replacing the running copy with a release that [`crate::updates`] already
//! downloaded and verified.
//!
//! A running program cannot overwrite itself, so each system hands the swap to
//! a helper that waits for this process to exit, puts the new copy in place, and
//! starts it. What "the copy" is differs:
//!
//! - macOS: the `.app` bundle, swapped whole.
//! - Linux: the folder holding the executable and its sidecars (what
//!   `install.sh` puts under `~/.local/share/kubernaut`), swapped whole.
//! - Windows: the same folder, but its files are copied over, so the uninstaller
//!   the setup program wrote beside them survives.
//!
//! A folder is only replaced if it carries [`PACKAGE_MARKER`], which the bundle
//! scripts write, so a development build in `target/release` never replaces
//! itself. A folder this user cannot write to (a `.deb` under `/usr/lib`) is
//! reported as such, and the person updates it the way they installed it.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Bundle identifier of a genuine release, checked before one replaces this.
const BUNDLE_ID: &str = "dev.kubernaut.app";

/// A file naming the package and its version, shipped beside the executable on
/// Linux and Windows. It marks a folder as an install and lets an update check
/// it is the version that was announced.
pub const PACKAGE_MARKER: &str = "kubernaut.package";

/// The executable inside a Linux or Windows package.
const fn executable_name() -> &'static str {
    if cfg!(windows) {
        "kubernaut-ui.exe"
    } else {
        "kubernaut-ui"
    }
}

/// What this process would replace itself with an update: the `.app` on macOS,
/// the install folder elsewhere. `None` when it runs from neither, such as a
/// development build.
pub fn current_install() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        bundle_of(&executable)
    } else {
        folder_of(&executable)
    }
}

fn bundle_of(executable: &Path) -> Option<PathBuf> {
    // .../Name.app/Contents/MacOS/exe
    let app = executable.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

fn folder_of(executable: &Path) -> Option<PathBuf> {
    let folder = executable.parent()?;
    folder
        .join(PACKAGE_MARKER)
        .is_file()
        .then(|| folder.to_path_buf())
}

/// Unpack a verified package and check it is the app it claims to be. Returns
/// what [`install`] takes: the `.app` on macOS, the package folder elsewhere.
pub fn stage(package: &Path, directory: &Path, version: &str) -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") {
        stage_app(package, directory, version)
    } else {
        stage_folder(package, directory, version)
    }
}

fn stage_app(zip: &Path, directory: &Path, version: &str) -> Result<PathBuf, String> {
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

/// The archive tool: `tar` where it is on `PATH`, and the one Windows ships,
/// named in full so a different `tar` earlier on `PATH` (Git's) cannot take
/// over a drive letter for a host name.
fn tar() -> PathBuf {
    let system = std::env::var_os("SystemRoot")
        .filter(|_| cfg!(windows))
        .map(|root| Path::new(&root).join("System32").join("tar.exe"));
    system
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("tar"))
}

fn stage_folder(archive: &Path, directory: &Path, version: &str) -> Result<PathBuf, String> {
    let into = directory.join("package");
    std::fs::create_dir_all(&into).map_err(|e| e.to_string())?;
    // `.tar.gz` on Linux, `.zip` on Windows, whose tar reads both.
    let flags = if cfg!(windows) { "-xf" } else { "-xzf" };
    let status = Command::new(tar())
        .arg(flags)
        .arg(archive)
        .arg("-C")
        .arg(&into)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !status.success() {
        return Err("the download could not be unpacked".into());
    }
    // The archive holds one folder, named for the version.
    let mut folders = std::fs::read_dir(&into)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir());
    let folder = folders.next().ok_or("the download holds no app")?;
    if folders.next().is_some() {
        return Err("the download holds more than one app".into());
    }
    check_package(&folder, version)?;
    Ok(folder)
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

/// The folder is this app, at the version announced, with its executable.
pub fn check_package(folder: &Path, version: &str) -> Result<(), String> {
    let marker = std::fs::read_to_string(folder.join(PACKAGE_MARKER))
        .map_err(|_| "the download is not Kubernaut")?;
    let mut words = marker.split_whitespace();
    if words.next() != Some(BUNDLE_ID) {
        return Err("the download is not Kubernaut".into());
    }
    if words.next() != Some(version) {
        return Err("the download is not the version that was announced".into());
    }
    if folder.join(executable_name()).is_file() {
        Ok(())
    } else {
        Err("the download has no executable".into())
    }
}

/// Swap the staged app in once this process has exited, and open it. The
/// previous app is put back if the swap fails.
const SWAP_APP: &str = r#"
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

/// The same for a Linux install folder. The staged folder is usually on another
/// file system, so a failed move can leave a partial folder behind: that is
/// removed before the previous one is put back.
const SWAP_FOLDER: &str = r#"
pid="$1"; dir="$2"; new="$3"; staging="$4"
n=0
while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 150 ]; do sleep 0.2; n=$((n+1)); done
old="$dir.updating"
rm -rf "$old"
if mv "$dir" "$old"; then
  if mv "$new" "$dir"; then
    rm -rf "$old"
  else
    rm -rf "$dir"
    mv "$old" "$dir"
  fi
fi
case "$staging" in */kubernaut-update-*) rm -rf "$staging" ;; esac
nohup "$dir/kubernaut-ui" >/dev/null 2>&1 &
"#;

/// The same for a Windows install folder: a copy over the files, backed up first.
/// ASCII only, as Windows PowerShell reads a script file in the system code page.
const SWAP_WINDOWS: &str = r#"param($ProcessId, $Dir, $New, $Staging)
$ErrorActionPreference = 'Stop'
Wait-Process -Id $ProcessId -Timeout 30 -ErrorAction SilentlyContinue
$backup = "$Dir.updating"
try {
  if (Test-Path $backup) { Remove-Item -Recurse -Force $backup }
  Copy-Item $Dir $backup -Recurse
  # An antivirus scan can still hold the executable for a moment.
  for ($try = 1; $try -le 20; $try++) {
    try { Copy-Item "$New\*" $Dir -Recurse -Force; break }
    catch { if ($try -eq 20) { throw }; Start-Sleep -Milliseconds 500 }
  }
  Remove-Item -Recurse -Force $backup
} catch {
  if (Test-Path $backup) {
    Copy-Item "$backup\*" $Dir -Recurse -Force
    Remove-Item -Recurse -Force $backup
  }
}
if ($Staging -like '*kubernaut-update-*') { Remove-Item -Recurse -Force $Staging -ErrorAction SilentlyContinue }
try { Start-Process -FilePath (Join-Path $Dir 'kubernaut-ui.exe') -WorkingDirectory $Dir } catch {}
"#;

/// Hand the swap to a helper that outlives this process. The caller then quits.
/// `target` is what [`current_install`] returned and `staged` what [`stage`] did.
pub fn install(staged: &Path, target: &Path) -> Result<(), String> {
    let staging = staged.parent().ok_or("the staged app has no folder")?;
    let process_id = std::process::id();
    if cfg!(target_os = "macos") {
        // Writing is the real test of whether the folder may be changed.
        check_writable(target.parent(), "drag the new app over it yourself")?;
        spawn_detached(swap_command(SWAP_APP, process_id, target, staged, staging))
    } else if cfg!(windows) {
        check_writable(Some(target), "run the setup program of the new version")?;
        // The script goes in the staging folder, which it removes last.
        let script = staging.join("swap.ps1");
        std::fs::write(&script, SWAP_WINDOWS).map_err(|e| e.to_string())?;
        // The staged folder is the package's one top-level folder; the script
        // copies what is in it.
        spawn_detached(windows_swap_command(
            &script, process_id, target, staged, staging,
        ))
    } else {
        check_writable(
            target.parent(),
            "install the new version the way you installed this one",
        )?;
        spawn_detached(swap_command(
            SWAP_FOLDER,
            process_id,
            target,
            staged,
            staging,
        ))
    }
}

/// Whether a file can be made in `folder`; the error says what to do instead.
fn check_writable(folder: Option<&Path>, instead: &str) -> Result<(), String> {
    let folder = folder.ok_or("the app has no folder")?;
    tempfile::Builder::new()
        .prefix(".kubernaut-write-test")
        .tempfile_in(folder)
        .map(|_| ())
        .map_err(|_| {
            format!(
                "{} cannot be changed from here; {instead}",
                folder.display()
            )
        })
}

/// A helper that waits for `process_id` to exit before it swaps.
fn swap_command(
    script: &str,
    process_id: u32,
    target: &Path,
    staged: &Path,
    staging: &Path,
) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(process_id.to_string())
        .arg(target)
        .arg(staged)
        .arg(staging);
    command
}

fn windows_swap_command(
    script: &Path,
    process_id: u32,
    target: &Path,
    staged: &Path,
    staging: &Path,
) -> Command {
    let shell = std::env::var_os("SystemRoot")
        .map(|root| {
            Path::new(&root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell"));
    let mut command = Command::new(shell);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
        ])
        .arg("-File")
        .arg(script)
        .arg(process_id.to_string())
        .arg(target)
        .arg(staged)
        .arg(staging);
    command
}

fn spawn_detached(mut command: Command) -> Result<(), String> {
    command
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

/// No console window, and not part of this process's group.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn detach(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A package folder of `version` with an executable that records that it ran.
    fn package(folder: &Path, version: &str) {
        write(
            &folder.join(PACKAGE_MARKER),
            &format!("{BUNDLE_ID} {version}\n"),
        );
        write(
            &folder.join(executable_name()),
            "#!/bin/sh\necho started > \"$(dirname \"$0\")/ran\"\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                folder.join(executable_name()),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
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
    fn only_a_folder_with_the_marker_is_an_install() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("installed");
        package(&installed, "1.0.0");
        let executable = installed.join(executable_name());
        assert_eq!(folder_of(&executable), Some(installed));

        let build = root.path().join("target-release");
        write(&build.join(executable_name()), "");
        assert_eq!(folder_of(&build.join(executable_name())), None);
    }

    #[test]
    fn a_package_must_be_this_app_at_this_version() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("kubernaut-2.0.0");
        package(&folder, "2.0.0");
        assert!(check_package(&folder, "2.0.0").is_ok());
        assert!(check_package(&folder, "2.0.1").is_err(), "another version");

        write(&folder.join(PACKAGE_MARKER), "someone.else 2.0.0\n");
        assert!(check_package(&folder, "2.0.0").is_err(), "another app");

        package(&folder, "2.0.0");
        fs::remove_file(folder.join(executable_name())).unwrap();
        assert!(check_package(&folder, "2.0.0").is_err(), "no executable");

        assert!(check_package(&root.path().join("missing"), "2.0.0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_tarball_is_unpacked_and_checked() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        package(&source.join("kubernaut-2.0.0"), "2.0.0");
        let tarball = root.path().join("package.tar.gz");
        let made = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&source)
            .arg("kubernaut-2.0.0")
            .status()
            .unwrap();
        assert!(made.success());

        let work = root.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let staged = stage_folder(&tarball, &work, "2.0.0").unwrap();
        assert!(staged.join(executable_name()).is_file());

        let other = root.path().join("other");
        fs::create_dir_all(&other).unwrap();
        assert!(stage_folder(&tarball, &other, "2.0.1").is_err());
        assert!(stage_folder(&root.path().join("nothing"), &other, "2.0.0").is_err());
    }

    /// The pid of a process that has already ended, so the helper does not wait.
    fn gone() -> u32 {
        let mut child = if cfg!(windows) {
            Command::new("cmd").args(["/c", "exit"]).spawn().unwrap()
        } else {
            Command::new("true").spawn().unwrap()
        };
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[cfg(unix)]
    fn wait_for(path: &Path) -> bool {
        (0..100).any(|_| {
            let found = path.exists();
            if !found {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            found
        })
    }

    #[cfg(unix)]
    fn run_swap(script: &str, target: &Path, staged: &Path, staging: &Path) {
        let status = swap_command(script, gone(), target, staged, staging)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_is_swapped_for_the_new_one_and_started() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("share/kubernaut");
        package(&installed, "1.0.0");
        write(&installed.join("old-only.txt"), "old");
        let staging = root.path().join("kubernaut-update-test");
        let staged = staging.join("package/kubernaut-2.0.0");
        package(&staged, "2.0.0");
        write(&staged.join("new-only.txt"), "new");

        run_swap(SWAP_FOLDER, &installed, &staged, &staging);

        assert!(installed.join("new-only.txt").is_file());
        assert!(!installed.join("old-only.txt").exists());
        assert_eq!(
            fs::read_to_string(installed.join(PACKAGE_MARKER)).unwrap(),
            format!("{BUNDLE_ID} 2.0.0\n")
        );
        assert!(!root.path().join("share/kubernaut.updating").exists());
        assert!(!staging.exists(), "the staging folder is removed");
        assert!(wait_for(&installed.join("ran")), "the new copy was started");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_puts_the_old_folder_back() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("share/kubernaut");
        package(&installed, "1.0.0");
        write(&installed.join("old-only.txt"), "old");
        let staging = root.path().join("kubernaut-update-test");
        fs::create_dir_all(&staging).unwrap();
        // Nothing to swap in: the move fails after the old folder was set aside.
        let staged = staging.join("package/missing");

        run_swap(SWAP_FOLDER, &installed, &staged, &staging);

        assert!(installed.join("old-only.txt").is_file());
        assert!(!root.path().join("share/kubernaut.updating").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_copies_the_new_files_over_and_keeps_the_others() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("Kubernaut");
        package(&installed, "1.0.0");
        write(&installed.join("old-only.txt"), "old");
        write(&installed.join("unins000.exe"), "the uninstaller");
        let staging = root.path().join("kubernaut-update-test");
        let staged = staging.join("package").join("Kubernaut");
        package(&staged, "2.0.0");
        write(&staged.join("new-only.txt"), "new");
        let script = staging.join("swap.ps1");
        fs::write(&script, SWAP_WINDOWS).unwrap();

        let status = windows_swap_command(&script, gone(), &installed, &staged, &staging)
            .status()
            .unwrap();
        assert!(status.success());

        assert!(installed.join("new-only.txt").is_file());
        assert_eq!(
            fs::read_to_string(installed.join(PACKAGE_MARKER)).unwrap(),
            format!("{BUNDLE_ID} 2.0.0\n")
        );
        assert!(
            installed.join("unins000.exe").is_file(),
            "the uninstaller survives"
        );
        assert!(!root.path().join("Kubernaut.updating").exists());
        assert!(!staging.exists(), "the staging folder is removed");
    }
}
