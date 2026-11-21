//! boot / recovery image parsing and ramdisk unpacking
//!
//! Pure Rust, no external binary dependencies. Compression formats auto-detected:
//!   gzip (1f 8b) / lz4 legacy (02 21 4c 18) / lz4 frame (04 22 4d 18) /
//!   xz (fd 37 7a 58 5a 00) / uncompressed (pipe)
//!
//! Field layout follows AOSP bootable/recovery/bootimg.h (supports v0~v4 headers).
//! This module is an offline command and does not connect to a device.

use crate::color::Colorize;
use crate::format::boot::slice;

// Parsing helpers live below the command layer in `crate::format::boot`; re-export them
// so legacy `crate::cmd::bootimg::*` paths keep working unchanged.
#[allow(unused_imports)]
pub use crate::format::boot::{
    BootImage, CpioEntry, Version, classify, decompress, lz4_block_decompress, parse, parse_cpio,
};

// ── Command entry ──

/// boot <boot.img> [--extract `<dir>`]
/// Parse a boot/recovery image, extract kernel/ramdisk/dtbo/dtb, decompress the ramdisk, list cpio entries, and classify boot/recovery.
pub fn cmd_boot(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file: Option<String> = None;
    let mut extract: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--extract" => {
                extract = args.get(i + 1).cloned();
                i += 2;
            }
            other if !other.starts_with('-') && file.is_none() => {
                file = Some(other.to_string());
                i += 1;
            }
            _ => {
                return Err(format!(
                    "未知参数: {}（用法: boot <boot.img> [--extract <dir>]）",
                    args[i]
                )
                .into());
            }
        }
    }
    let file = file.ok_or("缺少 boot.img 路径（用法: boot <boot.img> [--extract <dir>]）")?;

    let buf = std::fs::read(&file).map_err(|e| format!("读取 {} 失败: {}", file, e))?;
    let bi = parse(&buf)?;

    println!("{}", "==== boot 头部 ====".bold());
    println!("  版本        : {}", bi.version);
    println!("  page_size   : {}", bi.page_size);
    println!(
        "  kernel      : off={} size={} load=0x{:08X}",
        bi.kernel_off, bi.kernel_size, bi.kernel_load
    );
    println!(
        "  ramdisk     : off={} size={} load=0x{:08X}",
        bi.ramdisk_off, bi.ramdisk_size, bi.ramdisk_load
    );
    if bi.second_size > 0 {
        println!(
            "  second      : off={} size={}",
            bi.second_off, bi.second_size
        );
    }
    if bi.tags_load != 0 {
        println!("  tags_load   : 0x{:08X}", bi.tags_load);
    }
    if bi.recovery_dtbo_size > 0 {
        println!(
            "  recovery_dtbo: off={} size={}",
            bi.recovery_dtbo_offset, bi.recovery_dtbo_size
        );
    }
    if bi.dtb_size > 0 {
        println!("  dtb         : off={} size={}", bi.dtb_offset, bi.dtb_size);
    }
    println!("  name        : {}", bi.name);
    println!("  cmdline     : {}", bi.cmdline);

    // Extract
    let outdir = if let Some(ref d) = extract {
        std::fs::create_dir_all(d).map_err(|e| format!("创建目录 {} 失败: {}", d, e))?;
        Some(std::path::PathBuf::from(d))
    } else {
        None
    };

    let write_extract = |name: &str, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref dir) = outdir {
            let p = dir.join(name);
            std::fs::write(&p, data).map_err(|e| format!("写出 {} 失败: {}", p.display(), e))?;
            println!("  {}", format!("已抽取 -> {}", p.display()).dimmed());
        }
        Ok(())
    };

    let kernel = slice(&buf, bi.kernel_off, bi.kernel_size)?;
    write_extract("kernel.bin", kernel)?;

    let ramdisk_raw = slice(&buf, bi.ramdisk_off, bi.ramdisk_size)?;
    write_extract("ramdisk.raw", ramdisk_raw)?;

    if bi.recovery_dtbo_size > 0 {
        let dtbo = slice(&buf, bi.recovery_dtbo_offset, bi.recovery_dtbo_size)?;
        write_extract("recovery_dtbo.bin", dtbo)?;
    }
    if bi.dtb_size > 0 {
        let dtb = slice(&buf, bi.dtb_offset, bi.dtb_size)?;
        write_extract("dtb.bin", dtb)?;
    }

    if bi.ramdisk_size == 0 {
        println!("{}", "无 ramdisk（跳过解包）".yellow());
        return Ok(());
    }

    println!("{}", "==== ramdisk 解压 ====".bold());
    let ramdisk = decompress(ramdisk_raw).map_err(|e| format!("ramdisk 解压失败: {}", e))?;
    write_extract("ramdisk.cpio", &ramdisk)?;
    let entries = parse_cpio(&ramdisk).map_err(|e| format!("cpio 解析失败: {}", e))?;
    println!("  cpio 条目数: {}", entries.len());
    for e in &entries {
        println!(
            "    {:>5o} {:>5}:{:<5} {:<24} {} bytes",
            e.mode & 0o7777,
            e.uid,
            e.gid,
            e.name,
            e.data.len()
        );
    }
    let kind = classify(&entries);
    println!(
        "{}",
        format!("判定: 这是一个 **{}** 镜像", kind).green().bold()
    );
    Ok(())
}
