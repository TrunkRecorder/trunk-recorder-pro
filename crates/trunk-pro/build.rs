fn main() {
    // rust-embed reads web/dist at compile time; rebuild when it changes.
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=../../packaging/icons/trunk-pro.ico");
    // Windows: the icon and version information (Explorer's Properties tab).
    // Needs the Windows resource compiler, so only when building on Windows.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/trunk-pro.ico");
        res.set("ProductName", "Trunk Recorder Pro");
        res.set("FileDescription", "Trunk Recorder Pro — P25, SmartNet and DMR recorder");
        res.set("LegalCopyright", "GPL-3.0-or-later");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no Windows resources (icon, version info): {e}");
        }
    }
}
