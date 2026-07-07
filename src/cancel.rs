// Ctrl+C 取消状态管理
//
// 两次 Ctrl+C 机制：
// - 第一次：设置 `CANCEL_REQUESTED`，代码应在合适时机检查并优雅退出
// - 第二次：设置 `FORCE_REQUESTED`，代码应立即中断并退出

use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);
static FORCE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 安装 Ctrl+C 处理器
pub fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(move || {
        if CANCEL_REQUESTED.swap(true, Ordering::SeqCst) {
            // 第二次 Ctrl+C：强制取消
            FORCE_REQUESTED.store(true, Ordering::SeqCst);
            eprintln!("再次收到 Ctrl+C，正在强制退出...");
        } else {
            // 第一次 Ctrl+C：请求取消
            eprintln!("\n收到 Ctrl+C，正在取消...");
        }
    });
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
