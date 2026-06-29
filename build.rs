use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let profile = env::var("PROFILE").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let target_dir = PathBuf::from(&manifest_dir).join("target").join(&profile);
    fs::create_dir_all(&target_dir).ok();

    // 拷贝 libusb.exe（用于 reset 命令）
    let libusb_src = PathBuf::from(&manifest_dir)
        .join("binaries")
        .join("drivers")
        .join("libusb.exe");
    if libusb_src.exists() {
        let dest = target_dir.join("libusb.exe");
        fs::copy(&libusb_src, &dest).ok();
    }

    // 抑制 wdi-rs (libwdi) 的 linker 警告
    // LNK4098: LIBCMT 与其他库冲突（libwdi 静态库用 /MT 编译）
    // LNK4099: 静态库未附带 PDB 调试符号文件
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:LIBCMT");
    println!("cargo:rustc-link-arg=/IGNORE:4099");
}
