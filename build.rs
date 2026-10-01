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

    // 拷贝 data/ 资源目录（sdata.json + payload）到输出目录，使构建产物自包含。
    // data/ 采用 sdata.json 动态解析的 data/<芯片>/<文件> 布局（"高级存储方式"），
    // 运行时由 paths.rs 在 exe_dir/data/ 优先查找。
    let data_src = PathBuf::from(&manifest_dir).join("data");
    if data_src.is_dir() {
        let data_dst = target_dir.join("data");
        copy_dir_recursive(&data_src, &data_dst);
    }

    // 抑制 wdi-rs (libwdi) 的 linker 警告
    // LNK4098: LIBCMT 与其他库冲突（libwdi 静态库用 /MT 编译）
    // LNK4099: 静态库未附带 PDB 调试符号文件
    //
    // `/NODEFAULTLIB` 与 `/IGNORE` 是 MSVC link.exe 专属语法；若在非 MSVC 目标
    // （GNU/Linux/macOS）上传递，会被当作输入文件报错 "cannot open /NODEFAULTLIB:LIBCMT"。
    // libwdi 仅在 MSVC 目标下链接，故只在 target_env == "msvc" 时追加这两个参数。
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        println!("cargo:rustc-link-arg=/NODEFAULTLIB:LIBCMT");
        println!("cargo:rustc-link-arg=/IGNORE:4099");
    }
}

/// 递归拷贝目录（含子目录），已存在的文件直接覆盖。失败静默跳过单文件。
fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) {
    let _ = fs::create_dir_all(dst);
    let entries = match fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target);
        } else if fs::copy(&path, &target).is_ok() {
            // 拷贝成功
        }
    }
}
