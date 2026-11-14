//! 最小文件系统镜像生成（纯 Rust 自实现，无 mke2fs/make_f2fs 依赖）
//!
//! - [`ext4::create_minimal_ext4`]：可挂载的最小 ext4 镜像（空根目录）；
//! - [`f2fs::create_minimal_f2fs`]：可挂载的最小 f2fs 镜像（空根目录）。
//!
//! 用途：`zyb erase_data` 在擦除后对 userdata 格式化（erase + format），
//! 避免 bootloader 自动重建前分区处于无文件系统状态。

pub mod ext4;
pub mod f2fs;

/// 按分区名选择文件系统并生成最小镜像。
///
/// userdata 现代设备多用 f2fs；其余（metadata 等）不做格式化。
/// `partition_size` 为分区字节大小，生成镜像会被裁剪/填充到该大小。
pub fn format_image_for(partition: &str, partition_size: u64) -> Option<Vec<u8>> {
    let size = partition_size.clamp(1024 * 1024, 64 * 1024 * 1024 * 1024);
    match partition {
        "userdata" | "user" => Some(f2fs::create_minimal_f2fs(size)),
        _ => None,
    }
}
