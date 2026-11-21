//! Minimal filesystem image generation (pure Rust, no mke2fs/make_f2fs dependency)
//!
//! - [`ext4::create_minimal_ext4`]: mountable minimal ext4 image (empty root dir);
//! - [`f2fs::create_minimal_f2fs`]: mountable minimal f2fs image (empty root dir).
//!
//! Purpose: `zyb erase_data` formats userdata after erase (erase + format), so the
//! partition is not left without a filesystem before the bootloader rebuilds it.

pub mod ext4;
pub mod f2fs;

/// Generate a minimal image for an explicit filesystem type.
///
/// - `f2fs`: mainstream for modern userdata;
/// - `ext4`: legacy partitions such as metadata / cache. The generator is verified
///   mountable by e2fsck 1.47 (all 5 phases pass); see `fs/ext4/mod.rs` for the byte layout.
pub fn format_image_as(fs: &str, partition_size: u64) -> Result<Vec<u8>, String> {
    let size = partition_size.clamp(1024 * 1024, 64 * 1024 * 1024 * 1024);
    match fs.to_ascii_lowercase().as_str() {
        "f2fs" => Ok(f2fs::create_minimal_f2fs(size)),
        "ext4" => Ok(ext4::create_minimal_ext4(size)),
        other => Err(format!("不支持的文件系统: {}（仅 f2fs / ext4）", other)),
    }
}

/// Pick the filesystem by partition name (existing erase_data policy, behavior unchanged).
pub fn format_image_for(partition: &str, partition_size: u64) -> Option<Vec<u8>> {
    match partition {
        "userdata" | "user" => Some(f2fs::create_minimal_f2fs(
            partition_size.clamp(1024 * 1024, 64 * 1024 * 1024 * 1024),
        )),
        _ => None,
    }
}
