//! USB 连接诊断与状态检测
//!
//! 使用 nusb 设备枚举替代 libusb unsafe 代码

/// 检测 nusb 错误类型并给出诊断信息
pub fn classify_libusb_error(err: &str) -> &'static str {
    if err.contains("Timeout") || err.contains("timeout") {
        "超时 (设备未响应)"
    } else if err.contains("Pipe") || err.contains("pipe") {
        "端点 halt/stall — 需要清除"
    } else if err.contains("NotFound") || err.contains("not_found") {
        "设备未找到"
    } else if err.contains("Busy") || err.contains("busy") {
        "设备繁忙 — 可能被其他程序占用"
    } else if err.contains("Access") || err.contains("access") {
        "访问被拒绝 — 检查驱动"
    } else {
        "未知错误"
    }
}
