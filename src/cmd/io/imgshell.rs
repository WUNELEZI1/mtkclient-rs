//! boot / proinfo extension modes of fs_shell.
//!
//! Besides super, `fs_shell` also supports:
//! - `fs_shell boot [file]`   — interactively browse the ramdisk (cpio) of a boot/recovery image:
//!   `ls [path]` / `cat <file>` / `extract <dir>` / `info` / `quit`;
//!   when file is omitted, reads the boot partition for the current A/B slot (slot resolved automatically).
//! - `fs_shell proinfo [file]` — interactively inspect the NV data of the MTK proinfo partition:
//!   `show` / `raw <len>` / `quit`; when file is omitted, reads the proinfo partition from the device.
//!
//! Device-side reads go through `find_partition_addr` (handles A/B slot suffixes and special partitions).

use std::io::{BufRead as _, Write as _};

use crate::color::Colorize;
use crate::da::DAXFlash;
use log::info;

use crate::cmd::bootimg::{BootImage, decompress, parse as parse_boot, parse_cpio};
use crate::cmd::proinfo;

/// fs_shell extension entry: `cmd = "boot" | "proinfo"`.
pub fn cmd_imgshell(
    da: &mut DAXFlash,
    kind: &str,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    match kind {
        "boot" => shell_boot(da, args),
        "proinfo" => shell_proinfo(da, args),
        _ => Err(format!("未知 fs_shell 模式: {kind}").into()),
    }
}

/// Read data from a device partition or a local file.
fn load_source(
    da: &mut DAXFlash,
    partition: &str,
    file: Option<&String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if let Some(f) = file {
        std::fs::read(f).map_err(|e| format!("读取 {f} 失败: {e}").into())
    } else {
        let (addr, size) = da
            .find_partition_addr(partition)
            .map_err(|e| format!("查找 {partition} 分区失败: {e}（可显式指定文件路径）"))?;
        info!("读取 {partition} 分区: 0x{addr:X}, {} 字节", size);
        da.readflash_data(addr, size)
            .map_err(|e| format!("读取 {partition} 失败: {e}").into())
    }
}

// ═══════════════════════════════════════════════════════
// boot image shell
// ═══════════════════════════════════════════════════════

fn shell_boot(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let file = args.first();
    let buf = load_source(da, "boot", file)?;

    let bi = parse_boot(&buf).map_err(|e| format!("boot 头解析失败: {e}"))?;
    if bi.ramdisk_size == 0 {
        return Err("该 boot 镜像无 ramdisk（cpio 浏览不可用）".into());
    }
    let ramdisk = crate::format::boot::slice(&buf, bi.ramdisk_off, bi.ramdisk_size)?;
    let cpio = decompress(ramdisk).map_err(|e| format!("ramdisk 解压失败: {e}"))?;
    let entries = parse_cpio(&cpio).map_err(|e| format!("cpio 解析失败: {e}"))?;

    println!("{}", "═══ boot ramdisk (cpio) 浏览 ═══".cyan().bold());
    println!(
        "  镜像: {}",
        file.cloned().unwrap_or_else(|| "设备 boot 分区".into())
    );
    println!("  版本: {:?}  page_size: {}", bi.version, bi.page_size);
    println!("  cmdline: {}", bi.cmdline);
    println!("  cpio 条目: {}", entries.len());
    println!(
        "{}",
        "命令: ls [path] | cat <file> | extract <dir> | info | quit".dimmed()
    );

    let mut stdin = std::io::stdin().lock();
    loop {
        print!("{}", "boot> ".green().bold());
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();

        match cmd {
            "quit" | "exit" | "q" => break,
            "info" => {
                println!("  kernel: off={} size={}", bi.kernel_off, bi.kernel_size);
                println!("  ramdisk: off={} size={}", bi.ramdisk_off, bi.ramdisk_size);
                if bi.dtb_size > 0 {
                    println!("  dtb: off={} size={}", bi.dtb_offset, bi.dtb_size);
                }
                println!("  cpio 条目数: {}", entries.len());
            }
            "ls" => {
                let prefix = rest.first().copied().unwrap_or("");
                let shown = entries
                    .iter()
                    .filter(|e| prefix.is_empty() || e.name.starts_with(prefix))
                    .count();
                for e in entries
                    .iter()
                    .filter(|e| prefix.is_empty() || e.name.starts_with(prefix))
                {
                    let kind = if e.mode & 0o170000 == 0o040000 {
                        "d"
                    } else if e.mode & 0o170000 == 0o120000 {
                        "l"
                    } else {
                        "-"
                    };
                    println!(
                        "  {kind} {:>7} {:<40} {}",
                        e.data.len(),
                        e.name,
                        format!("{:04o}", e.mode & 0o7777).dimmed()
                    );
                }
                println!("{}", format!("共 {shown} 条").dimmed());
            }
            "cat" => {
                let name = rest.first().ok_or("用法: cat <file>")?;
                match entries.iter().find(|e| e.name == *name) {
                    Some(e) => {
                        std::io::stdout().write_all(&e.data).ok();
                        println!();
                    }
                    None => println!("{}", format!("未找到: {name}").red()),
                }
            }
            "extract" => {
                let dir = rest.first().copied().unwrap_or("boot_extract");
                std::fs::create_dir_all(dir)?;
                let mut n = 0usize;
                for e in &entries {
                    // Prevent path traversal: strip .. from entry names
                    let safe = e.name.replace("..", "_");
                    let p = std::path::Path::new(dir).join(safe.trim_start_matches('/'));
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    if std::fs::write(&p, &e.data).is_ok() {
                        n += 1;
                    }
                }
                println!("{}", format!("已导出 {n} 个条目到 {dir}/").green());
            }
            other => println!(
                "{}",
                format!("未知命令: {other}（ls/cat/extract/info/quit）").yellow()
            ),
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════
// proinfo shell
// ═══════════════════════════════════════════════════════

fn shell_proinfo(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let file = args.first();
    let buf = load_source(da, "proinfo", file)?;
    let analysis = proinfo::parse(&buf);

    println!("{}", "═══ proinfo 浏览 ═══".cyan().bold());
    println!(
        "  镜像: {}",
        file.cloned().unwrap_or_else(|| "设备 proinfo 分区".into())
    );
    println!("{}", "命令: show | raw <len> | quit".dimmed());

    let mut stdin = std::io::stdin().lock();
    loop {
        print!("{}", "proinfo> ".green().bold());
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next().unwrap_or("") {
            "quit" | "exit" | "q" => break,
            "show" => {
                println!(
                    "{}",
                    format!("==== proinfo 解析 ({} 字节) ====", buf.len()).bold()
                );
                if let Some(m) = &analysis.magic {
                    println!("  magic  : {m}");
                }
                if let Some(v) = analysis.version {
                    println!("  version: {v}");
                }
                if !analysis.blocks.is_empty() {
                    println!("  块表(有 magic):");
                    for b in &analysis.blocks {
                        println!(
                            "    {:<20} offset={:#x} length={}",
                            b.name, b.offset, b.length
                        );
                    }
                } else {
                    println!("  未识别到 PROINFO magic；启发式扫描候选:");
                    for b in &analysis.scan {
                        println!(
                            "    {:<20} offset={:#x} length={}",
                            b.name, b.offset, b.length
                        );
                    }
                    if analysis.scan.is_empty() {
                        println!("    (无候选，可能布局与假定格式不同)");
                    }
                }
            }
            "raw" => {
                let len: usize = parts.next().unwrap_or("256").parse().unwrap_or(256);
                let len = len.min(buf.len());
                dump_hex(&buf[..len]);
            }
            other => println!("{}", format!("未知命令: {other}（show/raw/quit）").yellow()),
        }
    }
    Ok(())
}

fn dump_hex(data: &[u8]) {
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {:08X}  {:<47}  {}", i * 16, hex.join(" "), ascii);
    }
}

/// `BootImage` is only dereferenced to keep the parse_boot return type consistent.
#[allow(dead_code)]
fn _boot_image_type_hint(b: &BootImage) -> &BootImage {
    b
}
