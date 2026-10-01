// Ctrl+C 取消状态管理
//
// 两次 Ctrl+C 机制：
// - 第一次：设置 `CANCEL_REQUESTED`，代码应在合适时机检查并优雅退出
// - 第二次：设置 `FORCE_REQUESTED`，代码应立即中断并退出

#[cfg(target_os = "windows")]
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);
static FORCE_REQUESTED: AtomicBool = AtomicBool::new(false);

// Ctrl+C 处理器的 Windows FFI 声明（替代 ctrlc crate：纯 FFI，无额外依赖）
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: extern "system" fn(u32) -> i32, add: i32) -> i32;
}

/// 控制台控制处理器：CTRL_C_EVENT(0) / CTRL_BREAK_EVENT(1) 触发两级取消逻辑，
/// 运行在控制台控制线程（非主线程），`eprintln` / `process::exit` 均安全。
#[cfg(target_os = "windows")]
extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1
    if ctrl_type == 0 || ctrl_type == 1 {
        if CANCEL_REQUESTED.swap(true, Ordering::SeqCst) {
            // 第二次 Ctrl+C：强制取消——真正终止进程，不做任何清理等待
            FORCE_REQUESTED.store(true, Ordering::SeqCst);
            eprintln!("再次收到 Ctrl+C，正在强制退出...");
            process::exit(130);
        } else {
            // 第一次 Ctrl+C：请求取消
            eprintln!("\n收到 Ctrl+C，正在取消...");
        }
        1
    } else {
        0
    }
}

/// 安装 Ctrl+C 处理器
///
/// 自研实现：通过 Windows API `SetConsoleCtrlHandler` 注册控制台控制处理器，
/// 替代第三方 `ctrlc` crate（纯 FFI，无额外依赖）。处理器运行在控制台控制线程，
/// `eprintln` / `process::exit` 均安全。两次 Ctrl+C 机制与原 `ctrlc` 实现一致：
/// 第一次设置 `CANCEL_REQUESTED`（优雅取消），第二次设置 `FORCE_REQUESTED` 并退出。
pub fn install_ctrlc_handler() {
    #[cfg(target_os = "windows")]
    unsafe {
        // add = 1 注册；控制台控制线程调用 ctrl_handler
        SetConsoleCtrlHandler(ctrl_handler, 1);
    }
    #[cfg(not(target_os = "windows"))]
    {
        // 项目仅面向 Windows；非 Windows 平台不注册 Ctrl+C 处理器
    }
}

/// 第一次 Ctrl+C 已按下（优雅取消）
pub fn requested() -> bool {
    CANCEL_REQUESTED.load(Ordering::SeqCst)
}

/// 第二次 Ctrl+C 已按下（强制取消）
pub fn force_requested() -> bool {
    FORCE_REQUESTED.load(Ordering::SeqCst)
}

/// 重置取消状态（用于内部复用场景）
pub fn reset() {
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    FORCE_REQUESTED.store(false, Ordering::SeqCst);
}
