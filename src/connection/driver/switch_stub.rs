//! WinUSB driver switching — stub implementation
//!
//! The real implementation lives in `switch.rs` and depends on wdi-rs (Windows only, needs libwdi).
//! This stub is used on non-Windows platforms or when the `winusb-driver` feature is disabled
//! (no libwdi toolchain/cross-compilation), so the crate compiles and `cargo test` runs (tests are pure logic, no driver install).

/// Switch the BROM device to the WinUSB driver
///
/// This build lacks the `winusb-driver` feature (or is not on Windows), so driver install is unsupported.
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
