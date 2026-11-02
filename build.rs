fn main() {
    for (header, name, prefix) in [
        ("native/audiocpp.h", "AudioApi", "audiocpp_.*"),
        ("native/manager.h", "ManagerApi", "rm_ac_.*"),
    ] {
        println!("cargo:rerun-if-changed={header}");
        bindgen::Builder::default()
            .header(header)
            .allowlist_function(prefix)
            .allowlist_type(prefix)
            .allowlist_var("AUDIOCPP_.*")
            .dynamic_library_name(name)
            .dynamic_link_require_all(true)
            .generate()
            .expect("generate pinned C ABI bindings")
            .write_to_file(
                std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap())
                    .join(format!("{name}.rs")),
            )
            .expect("write bindings");
    }
}
