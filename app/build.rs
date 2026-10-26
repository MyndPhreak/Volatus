fn main() {
    // Debug builds (and so the tests) carry the element info the headless window tests search
    // (`i_slint_backend_testing::ElementHandle`); a release build does not.
    let debug_info = std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug_info);
    slint_build::compile_with_config("ui/main.slint", config).expect("slint build");
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/volatus.ico");
    // Manifest: requestedExecutionLevel asInvoker (the app stays usable without admin).
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().unwrap();
}
