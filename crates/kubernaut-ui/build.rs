//! Puts the app icon and a manifest into the Windows executable. The icon is what
//! Explorer, the taskbar and the title bar show. The manifest makes Windows start
//! the app elevated (it asks once per launch), so the app can replace its own
//! files when it updates itself, wherever it was put. Does nothing on other
//! platforms, where the icon comes from the bundle or the desktop entry.

/// Run as administrator. Embedded as the executable's manifest.
#[cfg(windows)]
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set_manifest(MANIFEST);
        // A build without the icon is still a working build, so say so rather
        // than failing it. A build without the manifest is caught by
        // scripts/check-windows-crt.ps1, which CI and the release both run.
        if let Err(err) = resource.compile() {
            println!("cargo:warning=could not embed the Windows icon and manifest: {err}");
        }
    }
}
