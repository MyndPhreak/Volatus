fn main() {
    // Debug builds carry the element info the headless window tests search
    // (`i_slint_backend_testing::ElementHandle`); any build does with SLINT_EMIT_DEBUG_INFO set
    // (not "0"), as Slint's own option. Without it those tests are ignored (`slint_debug_info`), so
    // `cargo test --release` runs the rest. A shipped release build has no debug info.
    println!("cargo::rerun-if-env-changed=SLINT_EMIT_DEBUG_INFO");
    println!("cargo::rustc-check-cfg=cfg(slint_debug_info)");
    let asked = std::env::var("SLINT_EMIT_DEBUG_INFO").is_ok_and(|v| !v.is_empty() && v != "0");
    let debug_info = asked || std::env::var("PROFILE").is_ok_and(|p| p == "debug");
    if debug_info {
        println!("cargo::rustc-cfg=slint_debug_info");
    }
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug_info);
    slint_build::compile_with_config("ui/main.slint", config).expect("slint build");
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/volatus.ico");
    // Manifest: requestedExecutionLevel asInvoker (the app stays usable without admin).
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().unwrap();
}
