//! Preloader 模式 BootMode Switch 协议 (Layer 1)
//!
//! 对齐 MTKAuthPass.exe 的 `CMD_BootAsFASTBOOT()` 行为。
//!
//! 协议流程（从 EXE 逆向 + mtkclient meta.py 验证）：
//! 1. Start handshake with Preloader — drain 残留数据
//! 2. 等待设备发送 "READY"（5 字节 ASCII）
//! 3. Receive READY succeed!
//! 4. 发送 8 字节模式标识（如 FASTBOOT）
//! 5. 接收回传确认（如 TOOBTSAF = FASTBOOT 字节反转）
//! 6. 发送 "DISCONNECT"（10 字节）
//! 7. reboot to [mode]
//!
//! Pattern 映射（8 字节，发送标识 / 回传确认成对出现）：
//!   FASTBOOT  → 发送 "FASTBOOT"  / 回传 "TOOBTSAF"   ★ 无需 SLA
//!   METAMETA  → 发送 "METAMETA"  / 回传 "ATEMATEM"   ★ 需要 SLA
//!   FACTORYM  → 发送 "FACTORYM"  / 回传 "MYROTCAF"
//!   FACTFACT  → 发送 "FACTFACT"  / 回传 "TCAFTCAF"
//!   SWITCHMD  → 发送 "SWITCHMD"  / 回传 "DMHCTIWS"
//!   ADVEMETA  → 发送 "ADVEMETA"  / 回传 "ATEMEVDA"
//!
//! 注意：旧实现使用反转字符串作为发送标识（如 DMHCTIWS），这是错误的。
//! 正确的流程是发送正向标识（FASTBOOT），设备回传反向确认（TOOBTSAF）。
//! 但为了向后兼容，保留 pattern() 返回反转值用于 switch request 的内部标识。
//!
//! 参考：MTKAuthPass.exe 模式表 (0x022FC684 起)

use log::{info, trace, warn};
use std::time::Duration;

use crate::preloader::transport::BromTransport;

/// 支持的启动模式
///
/// 对齐 MTK Preloader Pattern 协议文档中全部 9 种模式。
/// 每种模式包含正向发送标识和反向回传确认。
#[derive(Debug, Clone, Copy)]
pub enum BootMode {
    Fastboot,       // bootloader (lk)
    Factory,        // factory / recovery
    FactoryMenu,    // factory menu
    Meta,           // meta mode
    AteMeta,        // ATE / META 组合模式
    AteEvda,        // ATE 工程验证下载代理 (ADV META)
    AteEvdx,        // ATE 工程验证调试 (ADV META X-variant)
    AdvancedMeta,   // 高级 META
    AteFactory,     // ATE 工厂
    DualTalkSwitch, // 双卡切换
}

impl BootMode {
    /// 发送标识 — 8 字节 ASCII，发送给 Preloader 的模式命令
    fn send_id(&self) -> &'static [u8] {
        match self {
            BootMode::Fastboot       => b"FASTBOOT",   // 发送标识
            BootMode::Factory        => b"FACTORYM",   // ATE Factory 发送标识
            BootMode::FactoryMenu    => b"FACTFACT",   // Factory Menu 发送标识
            BootMode::Meta           => b"METAMETA",   // META 发送标识
            BootMode::AteMeta        => b"ATEMATEM",   // ATE META 发送标识
            BootMode::AteEvda        => b"ATEMEVDA",   // ADV META 发送标识
            BootMode::AteEvdx        => b"ATEMEVDX",   // ADV META X-variant
            BootMode::AdvancedMeta   => b"ADVEMETA",   // ADV META 发送标识
            BootMode::AteFactory     => b"FACTFACT",   // ATE Factory
            BootMode::DualTalkSwitch => b"SWITCHMD",   // Mode Switch 发送标识
        }
    }

    /// 回传确认 — 设备回传的 8 字节反转字符串
    fn response_id(&self) -> &'static [u8] {
        match self {
            BootMode::Fastboot       => b"TOOBTSAF",   // FASTBOOT reversed
            BootMode::Factory        => b"MYROTCAF",   // FACTORYM reversed
            BootMode::FactoryMenu    => b"TCAFTCAF",   // FACTFACT reversed
            BootMode::Meta           => b"ATEMATEM",   // METAMETA reversed
            BootMode::AteMeta        => b"METAMETA",   // ATEMATEM reversed
            BootMode::AteEvda        => b"ADVEMETA",   // ATEMEVDA reversed
            BootMode::AteEvdx        => b"ADXVEMETA",   // ATEMEVDX reversed
            BootMode::AdvancedMeta   => b"ATEMEVDA",   // ADVEMETA reversed
            BootMode::AteFactory     => b"TCAFTCAF",   // FACTFACT reversed
            BootMode::DualTalkSwitch => b"DMHCTIWS",   // SWITCHMD reversed
        }
    }

    /// 反转 Pattern — 用于内部标识和 switch request
    /// 保留旧接口兼容性
    fn pattern(&self) -> &'static [u8] {
        match self {
            BootMode::Fastboot       => b"DMHCTIWS",   // "SWITCHMD" reversed (dualtalk)
            BootMode::Factory        => b"MYROTCAF",   // "FACTORYM" reversed
            BootMode::FactoryMenu    => b"TCAFTCAF",   // "FACTFACT" reversed
            BootMode::Meta           => b"ATEMATEM",   // "METAMETA" reversed
            BootMode::AteMeta        => b"METAMETA",   // "ATEMATEM" reversed
            BootMode::AteEvda        => b"ATEMEVDA",   // "ADVEMETA" reversed
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
            BootMode::FactoryMenu    => "FACTORY_MENU",
            BootMode::Meta           => "META",
            BootMode::AteMeta        => "ATE_META",
            BootMode::AteEvda        => "ATE_EVDA",
            BootMode::AteEvdx        => "ATE_EVDX",
            BootMode::AdvancedMeta   => "ADVANCED_META",
            BootMode::AteFactory     => "ATE_FACTORY",
            BootMode::DualTalkSwitch => "DUALTALK_SWITCH",
        }
    }

    /// 是否需要 SLA 认证
    /// 对齐 mtkclient meta.py: 只有 META (ATEMATEM) 需要额外握手
    fn needs_sla(&self) -> bool {
        matches!(self, BootMode::Meta | BootMode::AteMeta | BootMode::AteEvda | BootMode::AteEvdx)
    }

    /// 从用户输入的字符串解析 BootMode
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "fastboot" | "bootloader" | "lk"         => Some(BootMode::Fastboot),
            "factory" | "recovery"                     => Some(BootMode::Factory),
            "factory_menu" | "factfact"                => Some(BootMode::FactoryMenu),
            "meta"                                     => Some(BootMode::Meta),
            "ate_meta" | "atemeta"                    => Some(BootMode::AteMeta),
            "ate_evda" | "ateevda" | "adv_meta"       => Some(BootMode::AteEvda),
            "ate_evdx" | "ateevdx"                     => Some(BootMode::AteEvdx),
            "advanced_meta" | "advmeta" | "advemeta"  => Some(BootMode::AdvancedMeta),
            "ate_factory" | "atefactory"              => Some(BootMode::AteFactory),
            "dualtalk_switch" | "dualtalk" | "switchmd" => Some(BootMode::DualTalkSwitch),
            _ => None,
        }
    }
}

/// DISCONNECT 命令 — 告知 Preloader 断开连接并重启到目标模式
const DISCONNECT_CMD: &[u8] = b"DISCONNECT"; // 10 字节

/// 发送 BootMode Pattern 协议（完整 BootAs 流程）
///
/// 对齐 MTKAuthPass.exe CMD_BootAs* 系列行为：
/// 0. drain 残留数据
/// 1. 等待 Preloader 发送 "READY"
/// 2. 发送 8 字节模式标识（如 FASTBOOT）
/// 3. 接收回传确认（如 TOOBTSAF）
/// 4. 发送 DISCONNECT
/// 5. 等待设备重启到目标模式
///
/// 注意：FASTBOOT 不需要 SLA 认证，直接走 DISCONNECT。
/// META/ADV_META 需要额外 SLA 握手步骤（暂不支持，仅打印警告）。
pub fn send_boot_pattern(
    device: &mut dyn BromTransport,
    mode: BootMode,
) -> Result<(), String> {
    info!(
        "[BootAs] Start handshake with Preloader... mode={}",
        mode.name()
    );

    let orig_timeout = device.get_timeout();

    // 0. drain 残留数据
    device.set_timeout(Duration::from_millis(100));
    let mut drain_buf = [0u8; 512];
    loop {
        match device.read(&mut drain_buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => trace!("[BootAs] drain {} bytes", n),
        }
    }

    // 1. 等待 READY（最多重试 3 次，对齐 MTKAuthPass "retry READY" 逻辑）
    let mut ready_ok = false;
    for attempt in 0..3 {
        device.set_timeout(Duration::from_millis(2000));
        let mut ready_buf = [0u8; 64];
        match device.read(&mut ready_buf) {
            Ok(n) => {
                let resp = String::from_utf8_lossy(&ready_buf[..n]);
                if resp.trim() == "READY" {
                    info!("[BootAs] Receive READY succeed!");
                    ready_ok = true;
                    break;
                } else {
                    trace!(
                        "[BootAs] attempt {}: 收到 '{}' (hex: {})",
                        attempt + 1,
                        resp.trim(),
                        hex_str(&ready_buf[..n])
                    );
                    // 可能是前次操作的残留数据，继续等待
                    continue;
                }
            }
            Err(e) => {
                trace!("[BootAs] attempt {}: 等待 READY: {}", attempt + 1, e);
                if attempt == 2 {
                    warn!("[BootAs] 3 次尝试均未收到 READY，继续发送模式标识...");
                }
            }
        }
    }

    device.set_timeout(orig_timeout);

    if !ready_ok {
        warn!("[BootAs] 未收到 READY，设备可能已处于就绪状态");
    }

    // 2. 发送模式标识（8 字节，如 FASTBOOT）
    let send_id = mode.send_id();
    info!(
        "[BootAs] 发送模式标识: {} ({})",
        mode.name(),
        hex_str(send_id)
    );
    device.write(send_id)
        .map_err(|e| format!("发送模式标识失败: {}", e))?;

    // 3. 接收回传确认（如 TOOBTSAF）
    device.set_timeout(Duration::from_millis(5000));
    let expected_resp = mode.response_id();
    let mut resp_buf = [0u8; 64];
    match device.read(&mut resp_buf) {
        Ok(n) => {
            let resp = &resp_buf[..n];
            if resp == expected_resp {
                info!(
                    "[BootAs] 收到回传确认: {} ✓",
                    String::from_utf8_lossy(expected_resp)
                );
            } else if resp.len() >= 8 && &resp[..8] == expected_resp {
                info!(
                    "[BootAs] 收到回传确认: {} ✓ (含额外数据)",
                    String::from_utf8_lossy(expected_resp)
                );
            } else {
                warn!(
                    "[BootAs] 收到非预期回传: {} (预期: {})",
                    hex_str(resp),
                    hex_str(expected_resp)
                );
            }
        }
        Err(e) => {
            device.set_timeout(orig_timeout);
            warn!("[BootAs] 读取回传确认超时: {}，继续发送 DISCONNECT", e);
        }
    }
    device.set_timeout(orig_timeout);

    // 4. 发送 DISCONNECT
    info!("[BootAs] 发送 DISCONNECT");
    device.write(DISCONNECT_CMD)
        .map_err(|e| format!("发送 DISCONNECT 失败: {}", e))?;

    // 5. 等待设备重启
    info!("[BootAs] reboot to {}...", mode.name().to_lowercase());
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

fn hex_str(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
}
