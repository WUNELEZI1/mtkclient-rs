//! 路径管理模块
//!
//! 负责处理可执行文件相对路径的解析
//! 解决开发模式和发布模式的路径差异问题
//!
//! ## 目录结构
//! ```text
//! exe_dir/
//! ├── mtkclient-rs.exe
//! ├── data/                    ← 资源数据目录
//! │   ├── sdata.json
//! │   ├── MTK_DA_V5.bin
//! │   ├── mt6768/              ← 芯片专属 payload
//! │   │   └── mt6768_payload.bin
//! │   ├── mt6771/
//! │   │   └── mt6771_payload.bin
//! │   └── generic/             ← 通用 payload
//! │       ├── generic_dump_payload.bin
//! │       └── ...
//! └── tmp/                     ← 调试/缓存输出目录
//!     ├── .state
//!     ├── gpt.bin
//!     ├── gpt_full.bin
//!     └── usb_debug.log
//! ```
//!
//! ## --data-dir 覆盖机制
//! GUI (zybflashtool) 可通过 `--data-dir` 参数传入一个解压好的数据目录，
//! 设置后，路径解析优先从该目录查找。

use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 全局 data-dir 覆盖路径（由 GUI 通过 --data-dir 传入）
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 设置全局 data-dir（在 main 函数解析 CLI 后立即调用）
pub fn set_data_dir(path: PathBuf) {
    let _ = DATA_DIR.set(path);
}

/// 从 payload 文件名提取芯片目录名
///
/// 例如 `mt6768_payload.bin` → `Some("mt6768")`
fn extract_chip_name(file_name: &str) -> Option<&str> {
    file_name
        .strip_suffix("_payload.bin")
        .filter(|s| s.starts_with("mt") || s.starts_with("MT"))
}

/// 在指定基础目录中搜索文件（按优先级依次尝试）
///
/// 搜索顺序：
/// 1. 基础目录直接拼接（base/相对路径）
/// 2. base/data/ 直接拼接
/// 3. base/data/{chip_name}/ 芯片专属目录
/// 4. base/data/generic/ 通用 payload 目录
/// 5. base/payload/ 和 base/bin/ 旧兼容目录
fn search_in_base_dir(base_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    let path = Path::new(rel_path);

    // 1. 直接拼接
    let direct = base_dir.join(rel_path);
    if direct.exists() {
        return Some(direct);
    }

    let file_name = path.file_name()?.to_string_lossy();

    // 2. data/ 子目录（直接放文件）
    let data_direct = base_dir.join("data").join(file_name.as_ref());
    if data_direct.exists() {
        return Some(data_direct);
    }

    // 3. data/{chip_name}/ 芯片专属子目录
    if let Some(chip_name) = extract_chip_name(&file_name) {
        let chip_path = base_dir.join("data").join(chip_name).join(file_name.as_ref());
        if chip_path.exists() {
            return Some(chip_path);
        }
    }

    // 4. data/generic/ 通用 payload 子目录
    let generic_path = base_dir.join("data").join("generic").join(file_name.as_ref());
    if generic_path.exists() {
        return Some(generic_path);
    }

    // 5. payload/ 和 bin/ 旧兼容子目录
    let payload_path = base_dir.join("payload").join(file_name.as_ref());
    if payload_path.exists() {
        return Some(payload_path);
    }
    let bin_path = base_dir.join("bin").join(file_name.as_ref());
    if bin_path.exists() {
        return Some(bin_path);
    }

    None
}

/// 获取相对于可执行文件的路径
///
/// 搜索顺序：
/// 0. **--data-dir 覆盖**（GUI 传入的解压目录，最高优先级）
/// 1. 可执行文件同目录（exe_dir/相对路径）
/// 2. exe_dir/data/ 子目录（含芯片专属和通用 payload 查找）
/// 3. exe_dir/payload/ 或 exe_dir/bin/ 子目录（旧兼容）
/// 4. 开发模式：项目根目录（上翻 2 级 target/debug/ → target/ → 根目录）
/// 5. 回退：直接使用可执行文件目录拼接
pub fn get_exe_relative_path(rel_path: &str) -> PathBuf {
    // 0. --data-dir 覆盖（最高优先级）
    if let Some(data_dir) = DATA_DIR.get() {
        if let Some(found) = search_in_base_dir(data_dir, rel_path) {
            return found;
        }
    }

    if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        // 1~3. 在可执行文件目录中搜索（含 data/、payload/、bin/ 子目录）
        if let Some(found) = search_in_base_dir(exe_dir, rel_path) {
            return found;
        }

        // 4. 开发模式：上翻 2 级到项目根目录
        if let Some(project_root) = exe_dir.parent().and_then(|parent_dir| parent_dir.parent())
        {
            let dev_path = project_root.join(rel_path);
            if dev_path.exists() {
                return dev_path;
            }
            // 开发模式下也尝试 data/ 子目录
            if let Some(found) = search_in_base_dir(project_root, rel_path) {
                return found;
            }
        }

        // 5. 回退：直接返回拼接路径（即使不存在）
        return exe_dir.join(rel_path);
    }
    // 回退：使用当前工作目录
    PathBuf::from(rel_path)
}

/// 获取 tmp 目录下的文件路径
///
/// 自动创建 tmp/ 目录（位于可执行文件旁边）。
/// 用于存放调试输出、缓存文件等临时文件。
///
/// ```text
/// exe_dir/tmp/gpt.bin
/// exe_dir/tmp/.state
/// exe_dir/tmp/usb_debug.log
/// ```
pub fn get_tmp_path(file_name: &str) -> PathBuf {
    let tmp_dir = if let Some(data_dir) = DATA_DIR.get() {
        data_dir.join("tmp")
    } else if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        exe_dir.join("tmp")
    } else {
        PathBuf::from("tmp")
    };

    // 确保 tmp 目录存在
    if !tmp_dir.exists() {
        let _ = std::fs::create_dir_all(&tmp_dir);
    }

    tmp_dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exe_relative_path() {
        let path = get_exe_relative_path("usb_driver/test.inf");
        assert!(path.ends_with("usb_driver/test.inf"));
    }

    #[test]
    fn test_extract_chip_name() {
        assert_eq!(extract_chip_name("mt6768_payload.bin"), Some("mt6768"));
        assert_eq!(extract_chip_name("mt6771_payload.bin"), Some("mt6771"));
        assert_eq!(extract_chip_name("generic_dump_payload.bin"), None);
        assert_eq!(extract_chip_name("MTK_DA_V5.bin"), None);
    }
}
