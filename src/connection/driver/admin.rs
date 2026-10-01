//! 管理员权限检测与提权
//!
//! - `is_admin()` — 当前进程是否以管理员身份运行
//! - `restart_as_admin()` — 通过 PowerShell Start-Process -Verb RunAs 触发 UAC 提权，
//!   并设置 `MTKCLIENT_ELEVATED=1` 环境变量避免子进程再次提权造成死循环

#[cfg(target_os = "windows")]
use log::info;

/// 检查当前进程是否以管理员身份运行
///
/// 自研实现：通过 Windows API `OpenProcessToken` + `GetTokenInformation(TokenElevation)`
/// 查询当前进程令牌的提升状态，替代第三方 `is_elevated` crate（纯 FFI，无额外依赖）。
#[cfg(target_os = "windows")]
pub fn is_admin() -> bool {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            hprocess: *mut std::ffi::c_void,
            dwaccess: u32,
            htoken: *mut *mut std::ffi::c_void,
        ) -> i32;
        fn GetTokenInformation(
            tokenhandle: *mut std::ffi::c_void,
            tokeninformationclass: u32,
            tokendata: *mut std::ffi::c_void,
            tokendatasize: u32,
            returnsize: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn CloseHandle(hobject: *mut std::ffi::c_void) -> i32;
    }
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION: u32 = 20;
    unsafe {
        let mut token: *mut std::ffi::c_void = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: u32 = 0;
        let mut retsize: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION,
            &mut elevation as *mut u32 as *mut std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
            &mut retsize,
        );
        CloseHandle(token);
        ok != 0 && elevation != 0
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn is_admin() -> bool {
    true
}

/// 以管理员权限重启当前进程
///
/// 设置环境变量 `MTKCLIENT_ELEVATED=1` 给子进程，避免子进程再次触发提权造成死循环。
#[cfg(target_os = "windows")]
pub fn restart_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("获取 exe 路径失败: {}", e))?;
    let args: Vec<String> = std::env::args().skip(1).collect();

    info!("[DRIVER] 正在以管理员权限重启: {}", exe.display());

    let ps_cmd = format!(
        "$env:MTKCLIENT_ELEVATED='1'; Start-Process '{}' -ArgumentList '{}' -Verb RunAs -Wait",
        exe.display(),
        args.iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",")
    );

    let result = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_cmd])
        .status()
        .map_err(|e| format!("启动管理员进程失败: {}", e))?;

    if result.success() {
        std::process::exit(0);
    } else {
        Err("管理员重启失败（用户可能取消了 UAC）".to_string())
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn restart_as_admin() -> Result<(), String> {
    Ok(())
}
