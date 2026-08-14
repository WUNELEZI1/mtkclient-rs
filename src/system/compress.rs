//! 资源文件读取模块
//!
//! 程序直接从可执行文件所在目录（或其 `data/` 子目录）读取 payload / DA 等二进制资源，
//! 不再依赖 tar.gz 压缩包。路径解析统一由 [`crate::system::paths::获取可执行文件相对路径`]
//! 完成，支持 `exe_dir/data/`、`data/{chip}/`、`data/generic/`、`payload/`、`bin/` 等布局。

use log::debug;
use std::path::Path;

use crate::system::paths::获取可执行文件相对路径;

/// 读取资源文件（payload / DA / preloader dump 等）。
///
/// 查找顺序：
/// 1. 传入路径本身（调用点通常已通过 `获取可执行文件相对路径` 解析为绝对路径并存在）
/// 2. 按 `data/` 目录规则解析（兼容调用点传入裸文件名的情况）
pub fn read_file_auto_decompress(path: &Path) -> Result<Vec<u8>, String> {
    // 1. 原始路径（已解析的绝对路径或本就存在的相对路径）
    if path.exists() {
        debug!("读取资源文件: {}", path.display());
        return std::fs::read(path).map_err(|e| format!("读取文件失败 {}: {}", path.display(), e));
    }

    // 2. 兼容传入裸文件名：按 data/ 目录规则解析
    let resolved = 获取可执行文件相对路径(&path.to_string_lossy());
    if resolved.exists() {
        debug!("从 data 目录读取资源文件: {}", resolved.display());
        return std::fs::read(&resolved)
            .map_err(|e| format!("读取文件失败 {}: {}", resolved.display(), e));
    }

    Err(format!(
        "找不到资源文件: {}（已尝试原始路径与 data 目录解析）",
        path.display()
    ))
}
