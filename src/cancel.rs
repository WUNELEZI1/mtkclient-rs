// Ctrl+C cancellation state management
//
// Two-stage Ctrl+C mechanism:
// - First press: sets `CANCEL_REQUESTED`; code should check it at a safe point and exit gracefully
// - Second press: sets `FORCE_REQUESTED`; code should abort and exit immediately

#[cfg(target_os = "windows")]
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);
static FORCE_REQUESTED: AtomicBool = AtomicBool::new(false);

// Windows FFI declaration of the Ctrl+C handler (replaces the ctrlc crate: pure FFI, no extra deps)
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: extern "system" fn(u32) -> i32, add: i32) -> i32;
}

/// Console control handler: CTRL_C_EVENT(0) / CTRL_BREAK_EVENT(1) drive the two-stage cancellation.
/// Runs on a console control thread (not the main thread), so `eprintln` / `process::exit` are safe.
#[cfg(target_os = "windows")]
extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1
    if ctrl_type == 0 || ctrl_type == 1 {
        if CANCEL_REQUESTED.swap(true, Ordering::SeqCst) {
            // Second Ctrl+C: force cancel — terminate the process with no cleanup wait
            FORCE_REQUESTED.store(true, Ordering::SeqCst);
            eprintln!("再次收到 Ctrl+C，正在强制退出...");
            process::exit(130);
        } else {
            // First Ctrl+C: request cancellation
            eprintln!("\n收到 Ctrl+C，正在取消...");
        }
        1
    } else {
        0
    }
}

/// Install the Ctrl+C handler
///
/// Custom implementation: registers a console control handler via the Windows API
/// `SetConsoleCtrlHandler`, replacing the third-party `ctrlc` crate (pure FFI, no extra
/// dependencies). The handler runs on a console control thread, so `eprintln` /
/// `process::exit` are safe. The two-stage Ctrl+C mechanism matches the old `ctrlc` version:
/// the first press sets `CANCEL_REQUESTED` (graceful), the second sets `FORCE_REQUESTED` and exits.
pub fn install_ctrlc_handler() {
    #[cfg(target_os = "windows")]
    unsafe {
        // add = 1 registers; the console control thread then calls ctrl_handler
        SetConsoleCtrlHandler(ctrl_handler, 1);
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Windows-only project; no Ctrl+C handler is registered on other platforms
    }
}

/// First Ctrl+C has been pressed (graceful cancellation)
pub fn requested() -> bool {
    CANCEL_REQUESTED.load(Ordering::SeqCst)
}

/// Second Ctrl+C has been pressed (forced cancellation)
pub fn force_requested() -> bool {
    FORCE_REQUESTED.load(Ordering::SeqCst)
}

/// Reset the cancellation state (for internal reuse scenarios)
pub fn reset() {
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    FORCE_REQUESTED.store(false, Ordering::SeqCst);
}
