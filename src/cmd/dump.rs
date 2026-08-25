//! 镜像提取命令
//!
//! - `cmd_dumppreloader`— 提取 Preloader
//!   - DA 模式（Preloader 模式已加载 DA / BROM 已 bypass 并 upload_da）：
//!     直接通过 XFlash `read_data` 从设备内存（preloader 分区 addr=0）一次性读出 512KB，
//!     无需 Kamakiri BROM exploit —— 更快、更稳，且不受 BROM 内存读锁限制。
//!     对齐刷机匣（GeekFlashTool）的「尝试DA模式下导出Preloader」行为。
//!   - 纯 BROM 模式（DA 未加载）：回退到 Exploit 注入 payload 提取（原有路径）。

use crate::color::Colorize;
use log::{info, warn};

use crate::da::DAXFlash;
use crate::usb::UsbContext;

/// Preloader 在设备内存中的读出长度（512KB，足以覆盖 preloader 镜像 + BRLYT 头）
const PRELOADER_DUMP_SIZE: u64 = 0x80000;
/// 刷机匣实测：DA 模式读 preloader 用 parttype=1、addr=0，内容以 "EMMC_BOOT" 开头
const PRELOADER_DUMP_ADDR: u64 = 0x0;
const PRELOADER_DUMP_PARTTYPE: u32 = 0x1;
const PRELOADER_MAGIC: &[u8] = b"EMMC_BOOT";

/// 提取 Preloader
///
/// 优先走 DA 模式直接从设备内存读出（快、稳、无需 exploit）；
/// 仅当 DA 未加载（纯 BROM）或 DA 读出校验失败时，回退到 BROM Exploit 路径。
///
/// `context` 仅 BROM Exploit 回退路径需要（DA 直接读不依赖它）。DA 模式会话下
/// 设备已不在 BROM 态，无法走 exploit，故传 `None` 即可；若 DA 读出失败且
/// `context` 为 `None`，会明确报错而非静默失败。
pub fn cmd_dumppreloader(
    da: &mut DAXFlash,
    context: Option<&UsbContext>,
) -> Result<(), Box<dyn std::error::Error>> {
    if da.daext {
        match read_preloader_via_da(da) {
            Ok((data, filename)) => return save_preloader(data, filename),
            Err(e) => warn!(
                "DA 模式读取 Preloader 失败 ({}), 回退 BROM exploit 路径",
                e
            ),
        }
    }

    // 回退：BROM Exploit 注入 payload 提取（原有路径）
    // 直接调用 dump_preloader_payload，不先 bypass_security，
    // 因为 dump_preloader_payload 内部会注入 payload，bypass 会改变 BROM 状态
    let ctx = context.ok_or(
        "DUMPPRELOADER 在 DA 模式下读取失败，且无法回退到 BROM exploit（设备已在 DA 模式）",
    )?;
    let (data, filename) = da
        .preloader
        .dump_preloader_payload(ctx)
        .map_err(|e| format!("Exploit 提取失败: {}", e))?;
    save_preloader(data, filename)
}

/// DA 模式：用 XFlash read_data 从设备内存直接读出 preloader
fn read_preloader_via_da(da: &mut DAXFlash) -> Result<(Vec<u8>, String), String> {
    let data = da.readflash_data_ex(PRELOADER_DUMP_ADDR, PRELOADER_DUMP_SIZE, PRELOADER_DUMP_PARTTYPE)?;
    if data.len() < PRELOADER_MAGIC.len() || &data[0..PRELOADER_MAGIC.len()] != PRELOADER_MAGIC {
        return Err("读出数据缺少 EMMC_BOOT 头，可能非 Preloader".to_string());
    }
    let filename = extract_preloader_name(&data);
    Ok((data, filename))
}

/// 从 preloader 二进制中推测文件名（找第一个以 .bin 结尾且含 "preloader" 的可打印片段）
fn extract_preloader_name(data: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(data) {
        for w in s.split(|c: char| !(c.is_ascii_graphic() || c == ' ')) {
            if w.len() > 4 && w.ends_with(".bin") && w.contains("preloader") {
                return w.trim().to_string();
            }
        }
    }
    "preloader_dumped.bin".to_string()
}

/// 保存提取结果
fn save_preloader(data: Vec<u8>, filename: String) -> Result<(), Box<dyn std::error::Error>> {
    if data.is_empty() {
        warn!("提取完成但未收到有效数据");
        return Ok(());
    }
    let output = if filename.is_empty() {
        "preloader_dumped.bin".to_string()
    } else {
        filename
    };
    std::fs::write(&output, &data)?;
    info!(
        "{}",
        format!("Preloader 已提取并保存到: {}", output).green()
    );
    Ok(())
}
