fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "synoptic.gresource",
    );
    println!("cargo:rerun-if-changed=data");
}
