//! Resource file reading module
//!
//! The program reads binary resources such as payload / DA directly from the executable's
//! directory (or its `data/` subdirectory), no longer relying on tar.gz archives. Path
//! resolution is handled centrally by [`crate::system::paths::get_exe_relative_path`],
//! supporting layouts like `exe_dir/data/`, `data/{chip}/`, `data/generic/`, `payload/`, `bin/`, etc.

use log::debug;
use std::path::Path;

use crate::system::paths::get_exe_relative_path;

/// Read a resource file (payload / DA / preloader dump, etc.).
///
/// Lookup order:
/// 1. The given path itself (call sites usually pass an absolute path already resolved via `get_exe_relative_path` and present)
/// 2. Resolve using the `data/` directory rules (for call sites that pass a bare filename)
pub fn read_file_auto_decompress(path: &Path) -> Result<Vec<u8>, String> {
    // 1. Original path (already-resolved absolute path or an existing relative path)
    if path.exists() {
        debug!("读取资源文件: {}", path.display());
        return std::fs::read(path).map_err(|e| format!("读取文件失败 {}: {}", path.display(), e));
    }

    // 2. Support bare filenames: resolve using the data/ directory rules
    let resolved = get_exe_relative_path(&path.to_string_lossy());
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
