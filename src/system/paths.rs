//! 路径管理模块
//!
//! 负责处理可执行文件相对路径的解析
//! 解决开发模式和发布模式的路径差异问题

use std::env;
use std::path::PathBuf;

/// 获取相对于可执行文件的路径
///
/// 搜索顺序：
/// 1. 可执行文件同目录（exe_dir/payload/xxx.bin）
/// 2. 可执行文件同目录的 payload/ 或 bin/ 子目录（exe_dir/payload/xxx.bin）
/// 3. 开发模式：项目根目录（上翻 2 级 target/debug/ → target/ → 根目录）
/// 4. 回退：直接使用可执行文件目录拼接
pub fn 获取可执行文件相对路径(相对路径: &str) -> PathBuf {
    if let Ok(当前可执行文件) = env::current_exe()
        && let Some(可执行文件目录) = 当前可执行文件.parent()
    {
        // 1. 可执行文件同目录
        let 直接路径 = 可执行文件目录.join(相对路径);
        if 直接路径.exists() {
            return 直接路径;
        }

        // 2. 可执行文件目录下的 payload/ 或 bin/ 子目录
        //    支持 target/debug/payload/xxx.bin 结构
        if let Some(文件名) = PathBuf::from(相对路径).file_name() {
            let payload_path = 可执行文件目录.join("payload").join(文件名);
            if payload_path.exists() {
                return payload_path;
            }
            let bin_path = 可执行文件目录.join("bin").join(文件名);
            if bin_path.exists() {
                return bin_path;
            }
        }

        // 3. 开发模式：上翻 2 级到项目根目录
        if let Some(项目根目录) = 可执行文件目录.parent().and_then(|父目录| 父目录.parent())
        {
            let 开发路径 = 项目根目录.join(相对路径);
            if 开发路径.exists() {
                return 开发路径;
            }
        }

        // 4. 回退：直接返回拼接路径（即使不存在）
        return 直接路径;
    }
    // 回退：使用当前工作目录
    PathBuf::from(相对路径)
}

/// 英文别名（兼容旧调用）
pub fn exe_relative_path(rel_path: &str) -> PathBuf {
    获取可执行文件相对路径(rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exe_relative_path() {
        let path = 获取可执行文件相对路径("usb_driver/test.inf");
        assert!(path.ends_with("usb_driver/test.inf"));
    }
}
