//! 路径管理模块
//!
//! 负责处理可执行文件相对路径的解析
//! 解决开发模式和发布模式的路径差异问题

use std::env;
use std::path::PathBuf;

/// 获取相对于可执行文件的路径
///
/// # 参数
/// - `相对路径`: 相对于可执行文件的路径字符串
///
/// # 返回
/// 解析后的完整路径
///
/// # 说明
/// - 开发模式：可执行文件在 target/debug/ 或 target/release/，资源在项目根目录（需上翻 2 级）
/// - 发布模式：可执行文件和资源在同一目录，直接拼接
pub fn 获取可执行文件相对路径(相对路径: &str) -> PathBuf {
    if let Ok(当前可执行文件) = env::current_exe()
        && let Some(可执行文件目录) = 当前可执行文件.parent()
    {
        // 发布模式：可执行文件和资源在同一目录
        let 发布路径 = 可执行文件目录.join(相对路径);
        if 发布路径.exists() {
            return 发布路径;
        }
        // 开发模式：可执行文件在 target/debug/ 或 target/release/，资源在项目根目录
        // 上翻 2 级：target/debug/ → target/ → 项目根目录
        if let Some(项目根目录) = 可执行文件目录.parent().and_then(|父目录| 父目录.parent())
        {
            let 开发路径 = 项目根目录.join(相对路径);
            if 开发路径.exists() {
                return 开发路径;
            }
        }
        // 回退：直接使用可执行文件目录
        return 发布路径;
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
