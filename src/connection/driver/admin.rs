//! Administrator privilege detection and elevation.
//!
//! - `is_admin()` - whether the current process runs as administrator
//! - `restart_as_admin()` - triggers UAC elevation via PowerShell
//!   Start-Process -Verb RunAs, and sets `MTKCLIENT_ELEVATED=1` so the child
//!   process does not try to elevate again and loop forever

#[cfg(target_os = "windows")]
use log::info;

/// Checks whether the current process runs as administrator.
///
/// Self-contained implementation: queries the process token elevation via the
/// Windows APIs `OpenProcessToken` + `GetTokenInformation(TokenElevation)`,
/// replacing the third-party `is_elevated` crate (pure FFI, no extra dependency).
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

// `is_admin` / `restart_as_admin` do not exist on non-Windows platforms: both are
// Windows-only (is_admin uses Win32 token queries, restart_as_admin uses PowerShell
// UAC elevation), and there are no non-Windows call sites (the call in main.rs is
// also inside a `#[cfg(target_os = "windows")]` branch). They were therefore removed
// per the "delete placeholder stubs without real call sites" rule to avoid dead_code.

/// Restarts the current process with administrator privileges.
///
/// Sets the `MTKCLIENT_ELEVATED=1` environment variable for the child process so it
/// does not trigger elevation again and loop forever.
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
