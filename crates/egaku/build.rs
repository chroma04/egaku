//! Gives the Windows executable its icon and version information.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/egaku.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/egaku.ico");
        res.set("ProductName", "egaku");
        res.set("FileDescription", "egaku");
        if let Err(e) = res.compile() {
            // A missing resource compiler shouldn't stop the build.
            println!("cargo:warning=no Windows icon: {e}");
        }
    }
}
