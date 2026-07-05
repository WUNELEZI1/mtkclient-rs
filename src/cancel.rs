use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);
static FORCE_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        if CANCEL_REQUESTED.swap(true, Ordering::SeqCst) {
            FORCE_REQUESTED.store(true, Ordering::SeqCst);
            eprintln!("再次收到 Ctrl+C，正在取消 USB 传输并退出...");
            return;
        }
        eprintln!("收到 Ctrl+C，正在等待当前数据包写入完成后安全停止...");
    });
}

pub fn requested() -> bool {
    CANCEL_REQUESTED.load(Ordering::SeqCst)
}

pub fn force_requested() -> bool {
    FORCE_REQUESTED.load(Ordering::SeqCst)
}

pub fn reset_for_tests() {
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
    FORCE_REQUESTED.store(false, Ordering::SeqCst);
}
