//! Puts the app icon into the Windows executable, so Explorer, the taskbar and
//! the title bar show it. Does nothing on other platforms, where the icon
//! comes from the bundle or the desktop entry.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        // A build without the icon is still a working build, so say so rather
        // than failing it.
        if let Err(err) = resource.compile() {
            println!("cargo:warning=could not embed the Windows icon: {err}");
        }
    }
}
