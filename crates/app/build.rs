//! Embeds the INTmouse icon and version info in the exe (Explorer, taskbar pins,
//! Task Manager). The runtime window and tray icons are drawn in `src/gui/icon.rs`.

fn main() {
    println!("cargo:rerun-if-changed=assets/intmouse.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/intmouse.ico");
    res.set("ProductName", "INTmouse");
    res.set("FileDescription", "INTmouse");
    res.set("OriginalFilename", "INTmouse.exe");
    res.compile().expect("embed exe icon and version info");
}
