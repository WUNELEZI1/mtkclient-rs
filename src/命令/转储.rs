//! 镜像提取命令
//!
//! - `cmd_dumppreloader`— 提取 Preloader（通过 Exploit 注入 payload）

use colored::Colorize;
use log::{info, warn};

use crate::DA扩展::DAXFlash;
use crate::USB通信::USB上下文;

/// 提取 Preloader（mtkclient 风格）
/// 流程：
/// 1. 注入 generic_preloader_dump_payload.bin（ack=0xC1C2C3C4）
/// 2. USB 读取 4 字节长度头（小端 u32）
/// 3. USB 读取 Preloader 数据
/// 4. 搜索 MTK_BLOADER_INFO 提取文件名（偏移 0x1B，长度 0x30）
pub fn cmd_dumppreloader(
    da: &mut DAXFlash,
    context: &USB上下文,
) -> Result<(), Box<dyn std::error::Error>> {
    // 直接调用 dump_preloader_payload，不先 bypass_security
    // 因为 dump_preloader_payload 内部会注入 payload，bypass 会改变 BROM 状态
    let (data, filename) = da
        .preloader
        .dump_preloader_payload(false, false, context)
        .map_err(|e| format!("Exploit 提取失败: {}", e))?;

    if !data.is_empty() {
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
    } else {
        warn!("提取完成但未收到有效数据");
    }
    Ok(())
}
