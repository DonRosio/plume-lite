//! Embeds the icon and version information in plume-lite.exe, so that Explorer, the taskbar
//! and pinned shortcuts show the quill.

fn main() {
    println!("cargo:rerun-if-changed=assets/plume-lite.ico");
    // Only when building *on* Windows *for* Windows: the resource compiler (rc.exe) comes
    // with the Visual Studio C++ tools. Builds on Linux skip this step.
    let target_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    #[cfg(windows)]
    if target_windows {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/plume-lite.ico")
            .set("ProductName", "Plume Lite")
            .set("FileDescription", "Plume Lite")
            .set("LegalCopyright", "Plume Lite n'est pas affilié à Discord");
        if let Err(err) = resource.compile() {
            panic!("cannot embed the icon (rc.exe from the Visual Studio C++ tools is required): {err}");
        }
    }
    #[cfg(not(windows))]
    let _ = target_windows;
}
