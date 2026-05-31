fn main() {
    let zadig_dll = std::path::Path::new("zadig_rust.dll");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let target_dir = std::path::Path::new(&out_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    if zadig_dll.exists() {
        std::fs::copy(zadig_dll, target_dir.join("zadig_rust.dll"))
            .expect("Failed to copy zadig_rust.dll");
        println!("cargo:rerun-if-changed=zadig_rust.dll");
    }
}
