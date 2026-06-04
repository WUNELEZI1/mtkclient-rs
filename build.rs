use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let profile = env::var("PROFILE").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    
    let target_dir = PathBuf::from(&manifest_dir).join("target").join(&profile);
    fs::create_dir_all(&target_dir).ok();
    
    let src = PathBuf::from(&manifest_dir).join("binaries").join("drivers").join("libusb.exe");
    if src.exists() {
        let dest = target_dir.join("libusb.exe");
        fs::copy(&src, &dest).ok();
    }
}
