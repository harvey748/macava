// On macOS, compile the small Objective-C file that talks to ScreenCaptureKit, Core Audio and AppKit.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/platform/macos_shim.m");
        cc::Build::new()
            .file("src/platform/macos_shim.m")
            .flag("-fobjc-arc")
            .flag("-Wno-four-char-constants")
            .compile("aw_macos_shim");
        for framework in [
            "Foundation",
            "AppKit",
            "QuartzCore",
            "CoreGraphics",
            "CoreMedia",
            "CoreAudio",
            "ScreenCaptureKit",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
