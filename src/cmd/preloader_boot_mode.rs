//! Preloader 模式 BootMode Switch 协议
//!
//! 对齐 MABT (MTK Auth Bypass Tool) 的 `CMD_BootAsFASTBOOT()` 行为。
//!
//! 协议流程（从 mtk.exe 逆向）：
//! 1. Preloader 握手完成后，设备发送 "READY" 等待命令
//! 2. 发送 8 字节 Pattern（目标模式名称的反转字符串）
//! 3. 读取 Preloader 回复的 "READY"（5 字节 ASCII 确认）
//! 4. 发送 BootMode Switch Request 参数结构体
//! 5. 设备重启到目标模式
//!
//! Pattern 映射（8 字节，字符串反转）：
//!   FASTBOOT → "DMHCTIWS"
//!   FACTORY  → "MYROTCAF"
//!   META     → "ATEM    "（padded）
//!
//! Switch Request 参数（从 MABT 日志提取）：
//!   04 00 00 00 01 00 00 00 01 00 00 00  (12 bytes)
//!   04 00 00 00 01 00 00 00 01 00 00 C0  (12 bytes)
//!   06 00 00 00 01 00 00 00 01 00 00 C0 00 80 00 00 (16 bytes)
//!
//! 注意：串口独占，MABT 和本工具不能同时打开同一 COM 口。

use log::{info, trace, warn};
use std::time::Duration;

use crate::preloader::transport::BromTransport;

/// 支持的启动模式
///
/// 对齐 MTK Preloader Pattern 协议文档中全部 9 种模式。
/// Pattern 为 8 字节 ASCII 字符串（目标模式名称的反转或特定编码）。
#[derive(Debug, Clone, Copy)]
pub enum BootMode {
    Fastboot,       // bootloader (lk) — DMHCTIWS
    Factory,        // recovery — MYROTCAF
    Meta,           // meta mode — ATEM (padded)
    AteMeta,        // ATE / META 组合模式 — ATEMATEM
    AteEvda,        // ATE 工程验证下载代理 — ATEMEVDA
    AteEvdx,        // ATE 工程验证调试 — ATEMEVDX
    AdvancedMeta,    // 高级 META — ADVEMETA
    AteFactory,     // ATE 工厂 — FACTFACT
    DualTalkSwitch, // 双卡切换 — SWITCHMD
}

impl BootMode {
    /// 返回 8 字节 Pattern（反转字符串）
    fn pattern(&self) -> &[u8] {
        match self {
            BootMode::Fastboot       => b"DMHCTIWS",   // "FASTBOOT" reversed
            BootMode::Factory        => b"MYROTCAF",   // "FACTORYM" reversed
            BootMode::Meta           => b"ATEM    ",   // "META" padded to 8
            BootMode::AteMeta        => b"ATEMATEM",   // "ATEMATEM"
            BootMode::AteEvda        => b"ATEMEVDA",   // "ATEMEVDA"
            BootMode::AteEvdx        => b"ATEMEVDX",   // "ATEMEVDX"
            BootMode::AdvancedMeta   => b"ADVEMETA",   // "ADVEMETA"
            BootMode::AteFactory     => b"FACTFACT",   // "FACTFACT"
            BootMode::DualTalkSwitch => b"SWITCHMD",   // "SWITCHMD"
        }
    }

    fn name(&self) -> &'static str {
        match self {
            BootMode::Fastboot       => "FASTBOOT",
            BootMode::Factory        => "FACTORY",
            BootMode::Meta           => "META",
            BootMode::AteMeta        => "ATE_META",
            BootMode::AteEvda        => "ATE_EVDA",
            BootMode::AteEvdx        => "ATE_EVDX",
            BootMode::AdvancedMeta   => "ADVANCED_META",
            BootMode::AteFactory     => "ATE_FACTORY",
            BootMode::DualTalkSwitch => "DUALTALK_SWITCH",
        }
    }

    /// 从用户输入的字符串解析 BootMode
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "fastboot" | "bootloader" | "lk"         => Some(BootMode::Fastboot),
            "factory" | "recovery"                     => Some(BootMode::Factory),
            "meta"                                     => Some(BootMode::Meta),
            "ate_meta" | "atemeta"                    => Some(BootMode::AteMeta),
            "ate_evda" | "ateevda"                    => Some(BootMode::AteEvda),
            "ate_evdx" | "ateevdx"                    => Some(BootMode::AteEvdx),
            "advanced_meta" | "advmeta" | "advemeta"  => Some(BootMode::AdvancedMeta),
            "ate_factory" | "atefactory" | "factfact" => Some(BootMode::AteFactory),
            "dualtalk_switch" | "dualtalk" | "switchmd" => Some(BootMode::DualTalkSwitch),
            _ => None,
        }
    }
}

/// 发送 BootMode Pattern 协议
///
/// 流程：
/// 1. 发送 8 字节 Pattern
/// 2. 读取 "READY" 确认
/// 3. 发送 Switch Request 参数
pub fn send_boot_pattern(
    device: &mut dyn BromTransport,
    mode: BootMode,
) -> Result<(), String> {
    info!(
        "[Preloader BootMode] 发送 Pattern: {} ({})",
        mode.name(),
        hex_str(mode.pattern())
    );

    // 1. 发送 8 字节 Pattern
    device.write(mode.pattern())
        .map_err(|e| format!("发送 Pattern 失败: {}", e))?;
    trace!("[Preloader BootMode] Pattern 已发送");

    // 2. 读取 READY 确认（5 字节 ASCII）
    let mut ready_buf = [0u8; 5];
    match device.read_exact(&mut ready_buf) {
        Ok(_) => {
            let resp = String::from_utf8_lossy(&ready_buf);
            if resp == "READY" {
                info!("[Preloader BootMode] 收到 READY 确认");
            } else {
                warn!(
                    "[Preloader BootMode] 收到非 READY 响应: {} (hex: {})",
                    resp, hex_str(&ready_buf)
                );
            }
        }
        Err(e) => {
            warn!("[Preloader BootMode] 读取 READY 超时: {}，继续发送 Switch Request", e);
        }
    }

    // 3. 发送 BootMode Switch Request 参数结构体
    // 使用 MABT 日志中第一组参数（12 字节）作为默认值
    // 04 00 00 00 01 00 00 00 01 00 00 00
    let switch_req: [u8; 12] = [
        0x04, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00,
    ];
    info!("[Preloader BootMode] 发送 Switch Request: {}", hex_str(&switch_req));
    device.write(&switch_req)
        .map_err(|e| format!("发送 Switch Request 失败: {}", e))?;

    // 4. 等待设备断开（重启）
    info!("[Preloader BootMode] 等待设备重启...");
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

fn hex_str(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
}
