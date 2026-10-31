fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "ss-ui.gresource",
    );
    println!("cargo:rerun-if-changed=data");
}
