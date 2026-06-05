use log::{info, warn};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// 运行 install-filter.exe 为 BROM 设备安装 libusb-win32 filter
/// 
/// 流程：
/// 1. 找到 install-filter.exe（exe 同级 libusb/ 目录）
/// 2. 执行 install-filter.exe install --device=0x0E8D:0x0003
/// 3. 等待设备重枚举
pub fn install_libusb_filter() -> Result<(), String> {
    // 定位 install-filter.exe：exe 同级目录下的 libusb/install-filter.exe
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let filter_exe = exe_dir.join("libusb").join("install-filter.exe");

    if !filter_exe.exists() {
        // 回退：检查当前工作目录
        let cwd_filter = PathBuf::from("libusb").join("install-filter.exe");
        if cwd_filter.exists() {
            return run_filter_install(&cwd_filter);
        }
        return Err(format!(
            "install-filter.exe 未找到 (期望路径: {})",
            filter_exe.display()
        ));
    }

    run_filter_install(&filter_exe)
}

fn run_filter_install(exe_path: &PathBuf) -> Result<(), String> {
    info!(
        "安装 libusb-win32 filter: {} install --device=0x0E8D:0x0003",
        exe_path.display()
    );

    // 需要管理员权限
    if !is_admin() {
        warn!("未检测到管理员权限，install-filter 可能需要 UAC 提权");
    }

    let output = Command::new(exe_path)
        .args(["install", "--device=0x0E8D:0x0003"])
        .output()
        .map_err(|e| format!("执行 install-filter.exe 失败: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stdout.is_empty() {
        for line in stdout.lines() {
            info!("[filter] {}", line.trim());
        }
    }
    if !stderr.is_empty() {
        for line in stderr.lines() {
            warn!("[filter] {}", line.trim());
        }
    }

    if output.status.success() {
        info!("libusb-win32 filter 安装成功，等待设备重枚举...");
        std::thread::sleep(Duration::from_secs(3));
        Ok(())
    } else {
        // install-filter 对已安装的 filter 可能返回非零但仍成功
        if stdout.contains("already") || stdout.contains("成功") || stdout.contains("success") {
            info!("libusb-win32 filter 已存在");
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        } else {
            Err(format!(
                "install-filter.exe 退出码 {:?}，stdout: {}, stderr: {}",
                output.status.code(),
                stdout.trim(),
                stderr.trim()
            ))
        }
    }
}

/// 检查当前进程是否具有管理员权限
fn is_admin() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let output = Command::new("net")
            .arg("session")
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match output {
            Ok(o) => o.status.success(),
            Err(_) => false,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        unsafe { libc::getuid() == 0 }
    }
}
