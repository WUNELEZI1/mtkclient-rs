use libloading::{Library, Symbol};
use log::{debug, info};
use std::path::Path;
use std::process::Command;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

type ZadigDetectBootrom = unsafe extern "C" fn() -> i32;
type ZadigInstallEmbeddedDriver = unsafe extern "C" fn() -> i32;

/// 加载 zadig_rust.dll 并返回 Library 句柄和函数指针
fn load_zadig_lib() -> Result<
    (
        Library,
        Symbol<'static, ZadigDetectBootrom>,
        Symbol<'static, ZadigInstallEmbeddedDriver>,
    ),
    String,
> {
    let dll_path = Path::new("zadig_rust.dll");
    if !dll_path.exists() {
        return Err("zadig_rust.dll 未找到，请确保 DLL 与可执行文件在同一目录".to_string());
    }

    let lib = unsafe { Library::new(dll_path) }
        .map_err(|e| format!("加载 zadig_rust.dll 失败: {}", e))?;

    let detect: Symbol<'_, ZadigDetectBootrom> = unsafe {
        lib.get(b"zadig_detect_bootrom")
            .map_err(|e| format!("找不到 zadig_detect_bootrom: {}", e))?
    };
    let install: Symbol<'_, ZadigInstallEmbeddedDriver> = unsafe {
        lib.get(b"zadig_install_embedded_driver")
            .map_err(|e| format!("找不到 zadig_install_embedded_driver: {}", e))?
    };

    // 将 Symbol 的生命周期延长为 'static（安全：Library 句柄同时持有）
    let detect = unsafe { std::mem::transmute::<Symbol<'_, ZadigDetectBootrom>, Symbol<'static, ZadigDetectBootrom>>(detect) };
    let install = unsafe { std::mem::transmute::<Symbol<'_, ZadigInstallEmbeddedDriver>, Symbol<'static, ZadigInstallEmbeddedDriver>>(install) };

    Ok((lib, detect, install))
}

/// 安装 MediaTek BROM WinUSB 驱动
/// 通过 zadig_rust.dll 实现检测和安装
pub fn install_winusb_driver(debug: bool, force: bool) -> Result<(), String> {
    debug_log!(debug, "[DRV] install_winusb_driver start");

    if !force && check_driver() {
        info!("WinUSB 驱动已就绪");
        info!("使用 --force 可重新安装");
        return Ok(());
    }

    // 检查管理员权限
    if !is_admin() {
        info!("正在请求管理员权限...");
        return rerun_as_admin();
    }

    // 加载 zadig_rust.dll
    let (_lib, detect, install) = load_zadig_lib()?;

    // 检测 BROM 设备
    let detected = unsafe { detect() };
    if detected == 0 {
        return Err(
            "未检测到 BROM 设备。请按住音量键插入 USB 进入 BROM 模式后重试。".to_string(),
        );
    }

    info!("安装 MediaTek BROM WinUSB 驱动...");
    let result = unsafe { install() };
    if result == 1 {
        info!("驱动安装成功");
        info!("重新连接设备：");
        info!("  1. 关机");
        info!("  2. 按住音量加 + 音量减，插入 USB");
        info!("  3. 等待 BROM 设备识别");
        Ok(())
    } else {
        Err("驱动安装失败".to_string())
    }
}

/// 检查是否以管理员权限运行
fn is_admin() -> bool {
    use std::os::windows::process::CommandExt;
    let output = Command::new("net")
        .args(["session"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// 以管理员权限重新启动当前程序
fn rerun_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法获取 exe 路径: {}", e))?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    let status = Command::new("powershell")
        .args([
            "-Command",
            &format!(
                "Start-Process '{}' -ArgumentList '{}' -Verb RunAs -Wait",
                exe.display(),
                args.join(" ")
            ),
        ])
        .status()
        .map_err(|e| format!("提权失败: {}", e))?;

    if status.success() {
        std::process::exit(0);
    } else {
        Err("提权失败，请以管理员身份运行".to_string())
    }
}

pub fn check_driver() -> bool {
    let output = Command::new("pnputil").args(["/enum-drivers"]).output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("MediaTek") && (stdout.contains("WinUSB") || stdout.contains("oem"))
        }
        Err(_) => false,
    }
}
