use log::{debug, info, warn};
use std::path::PathBuf;
use std::process::Command;

const BROM_HWID: &str = r"USB\Vid_0E8D&Pid_0003";
const PRELOADER_HWID: &str = r"USB\Vid_0E8D&Pid_2000";

fn installer_path() -> Result<PathBuf, String> {
    let base = if cfg!(target_pointer_width = "64") {
        PathBuf::from("usb_driver/amd64/install-filter.exe")
    } else {
        PathBuf::from("usb_driver/x86/install-filter.exe")
    };
    if base.exists() {
        Ok(base)
    } else {
        Err(format!("未找到 libusb-win32 install-filter: {}", base.display()))
    }
}

fn run_install_filter(args: &[&str]) -> Result<String, String> {
    let exe = installer_path()?;
    let output = Command::new(&exe)
        .args(args)
        .output()
        .map_err(|e| format!("执行 {} 失败: {}", exe.display(), e))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stdout.is_empty() {
        debug!("[filter] stdout: {}", stdout);
    }
    if !stderr.is_empty() {
        debug!("[filter] stderr: {}", stderr);
    }
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "install-filter 执行失败 (code={:?}): {}{}",
            output.status.code(),
            stdout,
            stderr
        ))
    }
}

pub fn is_filter_installed() -> bool {
    [BROM_HWID, PRELOADER_HWID].iter().any(|hwid| {
        list_filter_for_device(hwid)
            .map(|out| out.to_lowercase().contains("libusb"))
            .unwrap_or(false)
    })
}

pub fn list_filter_for_device(hwid: &str) -> Result<String, String> {
    let arg = format!("--device={}", hwid);
    run_install_filter(&["list", &arg])
}

#[allow(dead_code)]
pub fn install_filter_driver(debug_mode: bool) -> Result<(), String> {
    let devices = [BROM_HWID, PRELOADER_HWID];
    for hwid in devices {
        let device_arg = format!("--device={}", hwid);
        let args = ["install", device_arg.as_str()];
        let res = run_install_filter(&args);
        match res {
            Ok(out) => {
                if debug_mode {
                    debug!("[filter] install result for {}: {}", hwid, out);
                }
                info!("libusb-win32 filter 已处理设备: {}", hwid);
            }
            Err(e) => {
                warn!("安装 filter 失败 ({}): {}", hwid, e);
            }
        }
    }
    if is_filter_installed() {
        Ok(())
    } else {
        Err("未检测到 libusb-win32 filter 已安装".to_string())
    }
}
