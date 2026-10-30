//! Build script: embed the application icon + version metadata into the
//! Windows executable (no-op on other platforms).

fn main() {
    // `cfg!` here means *host* == *target* for native builds (CI builds
    // natively); cross builds simply skip icon embedding.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/opacusr.ico");
        res.compile()
            .expect("failed to compile Windows resources (icon)");
    }
}
