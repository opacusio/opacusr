//! Compiles the ScreenCaptureKit Objective-C shim — on macOS targets
//! only (SCK does not exist on iOS; every other target has no use for
//! it either, so no `cc` dependency, no compile, no link flags there).
//!
//! The shim is the only Objective-C in the macOS capture stack; the
//! Rust side talks to it through the plain C API declared in
//! `src/macos_sck.rs`.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    println!("cargo:rerun-if-changed=shim/sck_shim.m");
    println!("cargo:rerun-if-changed=shim/cursor_shim.m");

    // Frameworks the shims themselves need. CoreGraphics/CoreFoundation
    // are already linked from macos.rs's #[link] attributes.
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=dylib=objc");

    cc::Build::new()
        .file("shim/sck_shim.m")
        .flag("-fobjc-arc")
        .compile("opacusr_sck_shim");

    cc::Build::new()
        .file("shim/cursor_shim.m")
        .flag("-fobjc-arc")
        .compile("opacusr_cursor_shim");
}
