fn main() {
    // The target, not the host: `cfg!` in a build script describes the machine
    // running it, so cross-checking Windows code from a Mac tried to link a
    // macOS framework into a Windows build.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=framework=UserNotifications");
    }
}
