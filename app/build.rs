fn main() {
    slint_build::compile("ui/main.slint").expect("slint build");
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    // Manifest: requestedExecutionLevel asInvoker (the app stays usable without admin).
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_required().unwrap();
}
