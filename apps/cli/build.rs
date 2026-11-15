fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=unikvm.manifest");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest_file("unikvm.manifest")
            .expect("embed the UniKVM Windows application manifest");
    }
}
