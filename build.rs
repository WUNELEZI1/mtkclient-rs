use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let profile = env::var("PROFILE").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let target_dir = PathBuf::from(&manifest_dir).join("target").join(&profile);
    fs::create_dir_all(&target_dir).ok();

    // 拷贝 libusb.exe（如果有）
    let libusb_src = PathBuf::from(&manifest_dir).join("binaries").join("drivers").join("libusb.exe");
    if libusb_src.exists() {
        let dest = target_dir.join("libusb.exe");
        fs::copy(&libusb_src, &dest).ok();
    }

    // 拷贝 libusb-filter 文件到 target/{profile}/libusb/
    // install-filter.exe, libusb0.dll, libusb0.sys
    println!("cargo:rerun-if-changed=binaries/libusb/install-filter.exe");
    println!("cargo:rerun-if-changed=binaries/libusb/libusb0.dll");
    println!("cargo:rerun-if-changed=binaries/libusb/libusb0.sys");

    let filter_dir = PathBuf::from(&manifest_dir).join("binaries").join("libusb");
    if filter_dir.exists() {
        let filter_dest = target_dir.join("libusb");
        fs::create_dir_all(&filter_dest).ok();
        for entry in fs::read_dir(&filter_dir).ok().into_iter().flatten().flatten() {
            let src_path = entry.path();
            if src_path.is_file() {
                let dest_path = filter_dest.join(entry.file_name());
                fs::copy(&src_path, &dest_path).ok();
            }
        }
    }
}
