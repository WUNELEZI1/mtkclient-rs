//! 最小文件系统镜像生成（纯 Rust 自实现，无 mke2fs/make_f2fs 依赖）
//!
//! - [`ext4::create_minimal_ext4`]：可挂载的最小 ext4 镜像（空根目录）；
//! - [`f2fs::create_minimal_f2fs`]：可挂载的最小 f2fs 镜像（空根目录）。
//!
//! 用途：`zyb erase_data` 在擦除后对 userdata 格式化（erase + format），
//! 避免 bootloader 自动重建前分区处于无文件系统状态。

pub mod ext4;
pub mod f2fs;

/// 显式指定文件系统类型生成最小镜像。
///
/// - `f2fs`：现代 userdata 主流；
/// - `ext4`：metadata / cache 等传统分区。生成器已由 e2fsck 1.47 验证可挂载
///   （5 阶段全过），字节布局见 `fs/ext4/mod.rs` 注释。
pub fn format_image_as(fs: &str, partition_size: u64) -> Result<Vec<u8>, String> {
    let size = partition_size.clamp(1024 * 1024, 64 * 1024 * 1024 * 1024);
    match fs.to_ascii_lowercase().as_str() {
        "f2fs" => Ok(f2fs::create_minimal_f2fs(size)),
        "ext4" => Ok(ext4::create_minimal_ext4(size)),
        other => Err(format!("不支持的文件系统: {}（仅 f2fs / ext4）", other)),
    }
}

/// 按分区名自动选择文件系统（erase_data 的既有策略，行为保持不变）。
pub fn format_image_for(partition: &str, partition_size: u64) -> Option<Vec<u8>> {
    match partition {
        "userdata" | "user" => Some(f2fs::create_minimal_f2fs(
            partition_size.clamp(1024 * 1024, 64 * 1024 * 1024 * 1024),
        )),
        _ => None,
    }
}
