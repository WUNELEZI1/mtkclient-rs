//! Kamakiri2 共享工具
//!
//! 提供跨子模块使用的 `debug_log!` 宏（按 `debug` 标志分发到 trace 日志）。
//! 复制自原 `kamakiri2.rs` 的 `debug_log!` 定义。

/// 按 `debug` 标志分发到 trace 日志
#[macro_export]
macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            ::log::trace!($($arg)*);
        }
    };
}
