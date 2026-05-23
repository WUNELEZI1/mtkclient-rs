use std::env;
use std::path::PathBuf;

/// 获取相对于资源根目录的路径
/// 解决开发模式和发布模式路径差异问题：
/// - 开发模式：exe 在 target/debug/ 或 target/release/，资源在项目根目录（需上翻 2 级）
/// - 发布模式：exe 和资源在同一目录，直接拼接
pub fn exe_relative_path(rel_path: &str) -> PathBuf {
    if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        // 发布模式：exe 和资源在同一目录
        let release_path = exe_dir.join(rel_path);
        if release_path.exists() {
            return release_path;
        }
        // 开发模式：exe 在 target/debug/ 或 target/release/，资源在项目根目录
        // 上翻 2 级：target/debug/ → target/ → 项目根目录
        if let Some(project_root) = exe_dir.parent().and_then(|p| p.parent()) {
            let dev_path = project_root.join(rel_path);
            if dev_path.exists() {
                return dev_path;
            }
        }
        // fallback: 直接用 exe_dir（路径存在但文件不存在的情况）
        return release_path;
    }
    // fallback: 当前工作目录
    PathBuf::from(rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exe_relative_path() {
        let path = exe_relative_path("usb_driver/test.inf");
        assert!(path.ends_with("usb_driver/test.inf"));
    }
}
