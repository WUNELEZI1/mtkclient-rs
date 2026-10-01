//! WinUSB 驱动切换 — 占位实现
//!
//! 真正的实现在 `switch.rs`，依赖 wdi-rs（仅支持 Windows，且需 libwdi 原生库）。
//! 当处于非 Windows 平台，或未启用 `winusb-driver` 特性（无 libwdi 工具链/交叉编译）
//! 时使用本实现，保证 crate 可编译并运行 `cargo test`（测试为纯逻辑，不需驱动安装）。

/// 切换 BROM 设备到 WinUSB 驱动
///
/// 当前构建未启用 `winusb-driver` 特性（或非 Windows 平台），不支持驱动安装。
pub fn switch_to_winusb() -> Result<(), String> {
    Err("未启用 winusb-driver 特性（或非 Windows 平台），不支持 WinUSB 驱动切换".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_reports_unsupported() {
        assert!(super::switch_to_winusb().is_err());
    }
}
