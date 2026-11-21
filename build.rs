use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let profile = env::var("PROFILE").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let target_dir = PathBuf::from(&manifest_dir).join("target").join(&profile);
    fs::create_dir_all(&target_dir).ok();

    // Copy libusb.exe (used by the reset command).
    let libusb_src = PathBuf::from(&manifest_dir)
        .join("binaries")
        .join("drivers")
        .join("libusb.exe");
    if libusb_src.exists() {
        let dest = target_dir.join("libusb.exe");
        fs::copy(&libusb_src, &dest).ok();
    }

    // Copy the data/ resources (sdata.json + payloads) so the build is self-contained.
    // Runtime lookup prefers exe_dir/data/ and resolves entries through sdata.json.
    let data_src = PathBuf::from(&manifest_dir).join("data");
    if data_src.is_dir() {
        let data_dst = target_dir.join("data");
        copy_dir_recursive(&data_src, &data_dst);
    }

    // Silence libwdi linker warnings:
    // LNK4098 (LIBCMT conflict, libwdi is built with /MT) and LNK4099 (missing PDB).
    //
    // `/NODEFAULTLIB` and `/IGNORE` are MSVC-only flags; on other targets they are
    // treated as input files. libwdi is linked on MSVC only, so gate them on target_env.
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        println!("cargo:rustc-link-arg=/NODEFAULTLIB:LIBCMT");
        println!("cargo:rustc-link-arg=/IGNORE:4099");
    }
}

/// Recursively copy a directory, overwriting existing files; per-file errors are ignored.
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
            // copied
        }
    }
}
