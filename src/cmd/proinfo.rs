//! MTK proinfo partition parser
//!
//! Note: MTK proinfo has no globally unified format; it varies widely by model and generation.
//! Two strategies are implemented:
//!    1) If the partition starts with the ASCII magic "PROINFO", parse as `version(u32) + count(u32)`
//!       followed by `{name[32], offset(u32), length(u32)}` block-table entries;
//!    2) Otherwise scan heuristically for candidate block-table entries of
//!       "32 printable bytes name + plausible offset/length".
//!
//! If the magic/layout differs on a real device, calibrate per model.
//!
//! This module is an offline command and does not connect to a device.

use crate::color::Colorize;

// The pure parser lives below the command layer in `crate::format::proinfo`; re-export it
// so legacy `crate::cmd::proinfo::*` paths keep working unchanged.
#[allow(unused_imports)]
pub use crate::format::proinfo::{ProInfo, ProInfoBlock, parse};

/// proinfo <proinfo.bin>
/// Parse an MTK proinfo partition.
pub fn cmd_proinfo(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let file = args
        .first()
        .ok_or("缺少 proinfo.bin 路径（用法: proinfo <proinfo.bin>）")?;
    let buf = std::fs::read(file).map_err(|e| format!("读取 {} 失败: {}", file, e))?;
    let info = parse(&buf);

    println!(
        "{}",
        format!("==== proinfo 解析 ({} 字节) ====", buf.len()).bold()
    );
    if let Some(m) = &info.magic {
        println!("  magic  : {}", m);
    }
    if let Some(v) = info.version {
        println!("  version: {}", v);
    }
    if !info.blocks.is_empty() {
        println!("  块表(有 magic):");
        for b in &info.blocks {
            println!(
                "    {:<20} offset={:#x} length={}",
                b.name, b.offset, b.length
            );
        }
    } else {
        println!("  未识别到 PROINFO magic；启发式扫描候选:");
        for b in &info.scan {
            println!(
                "    {:<20} offset={:#x} length={}",
                b.name, b.offset, b.length
            );
        }
        if info.scan.is_empty() {
            println!("    (无候选，可能布局与假定格式不同)");
        }
    }
    Ok(())
}
