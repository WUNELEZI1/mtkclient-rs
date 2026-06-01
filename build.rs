use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let profile = env::var("PROFILE").unwrap();
    
    // Go up from OUT_DIR (target/debug/build/mtkclient-rs-xxx/out) to target dir
    let target_dir = Path::new(&out_dir)
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("target"));
    
    let dest_dir = target_dir.join(&profile);
    fs::create_dir_all(&dest_dir).ok();
    
    // Copy libusb.exe to output directory
    let src = Path::new("binaries/drivers/libusb.exe");
    if src.exists() {
        let dest = dest_dir.join("libusb.exe");
        fs::copy(src, &dest).ok();
    }
}
