//! GPT 分区表相关命令
//!
//! - `cmd_printgpt`     — 打印 GPT 表到控制台 + 生成 scatter.txt
//! - `cmd_read_gpt`     — 读取 GPT 原始数据到目录
//! - `cmd_read_all`     — 读取全部分区到目录
//! - `cmd_print_scatter`— 打印 scatter 到屏幕并保存文件
//!
//! 私有助手：
//! - `print_gpt_table`  — 表格格式化输出

use colored::Colorize;
use log::{error, info, trace, warn};
use std::time::SystemTime;

use crate::DA分区::generate_scatter_from_gpt;
use crate::DA扩展::DAXFlash;
use crate::config::AppConfig;

/// 打印 GPT 分区表
pub fn cmd_printgpt(da: &mut DAXFlash, log_level: u8) {
    match da.read_gpt() {
        Ok(_) => {
            let mut boot1_size: u64 = 0;
            let mut boot2_size: u64 = 0;

            // 输出完整 EMMC 信息
            if let Ok(emmc_info) = da.get_emmc_info() {
                boot1_size = emmc_info.boot1_size;
                boot2_size = emmc_info.boot2_size;
                print_emmc_info(&emmc_info);
            } else {
                // 失败时降级到读 boot1/boot2
                if let Ok(simple_info) = da.get_emmc_info_simple() {
                    boot1_size = simple_info.boot1_size;
                    boot2_size = simple_info.boot2_size;
                    println!();
                    println!("{}", " EMMC 信息 (简化) ".on_yellow().black());
                    println!(
                        "  EMMC Boot1 Size: {}  {} MB",
                        format!("0x{:06X}", simple_info.boot1_size).green(),
                        format!("({} MB)", simple_info.boot1_size / 1024 / 1024).dimmed()
                    );
                    println!(
                        "  EMMC Boot2 Size: {}  {} MB",
                        format!("0x{:06X}", simple_info.boot2_size).green(),
                        format!("({} MB)", simple_info.boot2_size / 1024 / 1024).dimmed()
                    );
                }
            }

            if let Ok(data) = da.get_last_gpt_data() {
                print_gpt_table(data, boot1_size, boot2_size);
            }

            info!("{}", "GPT 读取成功".green());
            if log_level >= 3 {
                let ts = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let file = format!("gpt_debug_{}.bin", ts);
                if let Ok(data) = da.get_last_gpt_data() {
                    let _ = std::fs::write(&file, data);
                }
            }
            if let Ok(data) = da.get_last_gpt_data() {
                match generate_scatter_from_gpt(data, "scatter.txt") {
                    Ok(parts) => info!("  scatter.txt 已生成 ({} 分区)", parts.len()),
                    Err(e) => info!("  Warning: scatter 生成失败: {}", e),
                }
            }
        }
        Err(e) => error!("{}", format!("GPT 读取失败: {}", e).red()),
    }
}

/// 打印完整 EMMC 信息
fn print_emmc_info(info: &crate::DA扩展::EmmcInfo) {
    fn fmt_bytes(b: u64) -> String {
        if b >= 1_000_000_000 {
            format!("{} GB", b / 1_000_000_000)
        } else if b >= 1_000_000 {
            format!("{} MB", b / 1_000_000)
        } else if b >= 1_000 {
            format!("{} KB", b / 1_000)
        } else {
            format!("{} B", b)
        }
    }

    println!();
    println!("{}", " EMMC 信息 ".on_green().black());
    println!(
        "  存储类型:     {}",
        info.emmc_type.green()
    );
    if info.user_size > 0 {
        println!(
            "  用户区:       {}  ({} / {})",
            format!("{:>12}", info.user_size).green(),
            format!("{} GB", info.user_size / 1_000_000_000),
            format!("{} MB", info.user_size / 1_000_000)
        );
    }
    if info.boot1_size > 0 {
        println!(
            "  Boot1:        {}  ({})",
            format!("{:>12}", info.boot1_size).green(),
            fmt_bytes(info.boot1_size)
        );
    }
    if info.boot2_size > 0 {
        println!(
            "  Boot2:        {}  ({})",
            format!("{:>12}", info.boot2_size).green(),
            fmt_bytes(info.boot2_size)
        );
    }
    if info.rpmb_size > 0 {
        println!(
            "  RPMB:         {}  ({})",
            format!("{:>12}", info.rpmb_size).green(),
            fmt_bytes(info.rpmb_size)
        );
    }
    if info.block_size > 0 {
        println!(
            "  块大小:       {}  ({} 字节)",
            format!("0x{:X}", info.block_size).green(),
            info.block_size
        );
    }
    if !info.cid.is_empty() {
        let trimmed: Vec<u8> = info.cid.iter().copied().filter(|&b| b != 0).collect();
        if !trimmed.is_empty() {
            let cid_str: String = trimmed
                .iter()
                .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
                .collect();
            println!("  CID:          {}", cid_str.green());
        }
    }
}

/// 格式化字节数为人类可读字符串 (KB/MB/GB, 保留 2 位小数)
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bytes >= (1024 * 1024 * 1024) {
        format!("{:.2} GB", bytes as f64 / GB)
    } else if bytes >= (1024 * 1024) {
        format!("{:.2} MB", bytes as f64 / MB)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// 格式化字节数为带千分位的字符串
fn format_bytes_comma(bytes: u64) -> String {
    let s = bytes.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

/// 打印 GPT 表格到控制台（含 EMMC_BOOT_1/2 显示）
fn print_gpt_table(data: &[u8], boot1_size: u64, boot2_size: u64) {
    let gpt_info = match crate::DA分区::GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let partitions = gpt_info.partitions();
    let emmc_count = (if boot1_size > 0 { 1 } else { 0 }) + (if boot2_size > 0 { 1 } else { 0 });
    let total_rows = partitions.len() + emmc_count;

    // 列宽（地址按 0x%016X = 18 字符）
    const W_IDX: usize = 4;
    const W_NAME: usize = 22;
    const W_ADDR: usize = 18;
    const W_SIZE: usize = 16;
    const W_BYTES: usize = 20;
    const INNER_W: usize = W_IDX + 2 + W_NAME + 2 + W_ADDR + 2 + W_ADDR + 2 + W_SIZE + 2 + W_BYTES;

    // 标题框（先 format 出纯文本，再整体上色，避免 ANSI 破坏对齐）
    let title = format!(" GPT 分区表 ({} 个分区) ", total_rows);
    let pad = INNER_W.saturating_sub(title.len());
    let lpad = pad / 2;
    let rpad = pad - lpad;
    println!(" ╔{}╗", "═".repeat(INNER_W));
    println!(" ║{}{}{}║", " ".repeat(lpad), title, " ".repeat(rpad));
    println!(" ╚{}╝", "═".repeat(INNER_W));

    // 表头
    let header = format!(
        " {:<4}  {:<22} {:>18}  {:>18}  {:>16}  {:>20}",
        "编号", "名称", "起始地址", "结束地址", "大小", "字节数"
    );
    println!("{}", header.bright_white());

    // 分隔线
    println!(
        " {}  {}  {}  {}  {}  {}",
        "─".repeat(W_IDX),
        "─".repeat(W_NAME),
        "─".repeat(W_ADDR),
        "─".repeat(W_ADDR),
        "─".repeat(W_SIZE),
        "─".repeat(W_BYTES)
    );

    let mut row = 0usize;

    // EMMC Boot 区域（dimmed 灰色）
    if boot1_size > 0 {
        row += 1;
        let idx = format!("{:<4}", format!("{:02}", row));
        let name = format!("{:<22}", "boot1");
        let start = format!("{:>18}", format!("0x{:016X}", 0u64));
        let end = format!("{:>18}", format!("0x{:016X}", boot1_size.saturating_sub(1)));
        let size = format!("{:>16}", format_size(boot1_size));
        let bytes = format!("{:>20}", format_bytes_comma(boot1_size));
        println!(
            " {}  {}  {}  {}  {}  {}",
            idx.dimmed(),
            name.dimmed(),
            start.dimmed(),
            end.dimmed(),
            size.dimmed(),
            bytes.dimmed()
        );
    }
    if boot2_size > 0 {
        row += 1;
        let idx = format!("{:<4}", format!("{:02}", row));
        let name = format!("{:<22}", "boot2");
        let start = format!("{:>18}", format!("0x{:016X}", 0u64));
        let end = format!("{:>18}", format!("0x{:016X}", boot2_size.saturating_sub(1)));
        let size = format!("{:>16}", format_size(boot2_size));
        let bytes = format!("{:>20}", format_bytes_comma(boot2_size));
        println!(
            " {}  {}  {}  {}  {}  {}",
            idx.dimmed(),
            name.dimmed(),
            start.dimmed(),
            end.dimmed(),
            size.dimmed(),
            bytes.dimmed()
        );
    }

    // GPT 分区（交替亮度，先 format 定宽再上色）
    for (i, entry) in partitions.iter().enumerate() {
        row += 1;
        let start_addr = entry.start_addr;
        let end_addr = start_addr.saturating_add(entry.size).saturating_sub(1);
        let idx = format!("{:<4}", format!("{:02}", row));
        let name = format!("{:<22}", &entry.name);
        let start = format!("{:>18}", format!("0x{:016X}", start_addr));
        let end = format!("{:>18}", format!("0x{:016X}", end_addr));
        let size = format!("{:>16}", format_size(entry.size));
        let bytes = format!("{:>20}", format_bytes_comma(entry.size));

        if i % 2 == 0 {
            println!(
                " {}  {}  {}  {}  {}  {}",
                idx.cyan(),
                name.white(),
                start.green(),
                end.green(),
                size.yellow(),
                bytes.white()
            );
        } else {
            println!(
                " {}  {}  {}  {}  {}  {}",
                idx.bright_cyan(),
                name.bright_white(),
                start.bright_green(),
                end.bright_green(),
                size.bright_yellow(),
                bytes.bright_white()
            );
        }
    }

    // 底部分隔线
    println!(
        " {}  {}  {}  {}  {}  {}",
        "─".repeat(W_IDX),
        "─".repeat(W_NAME),
        "─".repeat(W_ADDR),
        "─".repeat(W_ADDR),
        "─".repeat(W_SIZE),
        "─".repeat(W_BYTES)
    );
    println!("  共 {} 个分区", row);
    println!();
}

/// 读取 GPT 原始数据到指定目录
pub fn cmd_read_gpt(
    da: &mut DAXFlash,
    dir: &str,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;
    let output = format!("{}/gpt.bin", dir);

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?;
    std::fs::write(&output, gpt_data).map_err(|e| format!("写入失败: {}", e))?;
    info!(
        "{}",
        format!("GPT 已保存: {} ({} 字节)", output, gpt_data.len()).green()
    );

    if log_level >= 2 {
        print_gpt_table(gpt_data, 0, 0);
    }

    Ok(())
}

/// 读取全部分区到目录（支持 --skip 跳过指定分区）
pub fn cmd_read_all(da: &mut DAXFlash, dir: &str, app_config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    // 解析 --skip 分区列表
    let skip_set: std::collections::HashSet<String> = match &app_config.skip_partitions {
        Some(skip_str) => {
            let set: std::collections::HashSet<String> = skip_str
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .collect();
            if !set.is_empty() {
                info!("跳过分区: {}", skip_str);
            }
            set
        }
        None => std::collections::HashSet::new(),
    };

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::DA分区::GptInfo::parse(&gpt_data)?;

    let mut read_count = 0usize;
    let mut skip_count = 0usize;

    for entry in gpt_info.iter_partitions() {
        // 跳过指定分区（大小写不敏感比较）
        if skip_set.contains(&entry.name.to_lowercase()) {
            info!("  [SKIP] {} (0x{:X})", entry.name, entry.size);
            skip_count += 1;
            continue;
        }

        let output = format!("{}/{}.img", dir, entry.name);
        info!(
            "  [{}/{}] 读取 {} (0x{:X} @ 0x{:X})",
            read_count + 1,
            gpt_info.partitions().len() - skip_count,
            entry.name,
            entry.size,
            entry.start_addr
        );
        let data = da
            .readflash_data(entry.start_addr, entry.size)
            .map_err(|e| format!("读取 {} 失败: {}", entry.name, e))?;
        std::fs::write(&output, &data).map_err(|e| format!("写入失败: {}", e))?;
        info!("{}", format!("  {} -> {}", entry.name, output).green());
        read_count += 1;
    }

    info!(
        "{}",
        format!(
            "分区读取完成: {} 成功, {} 跳过, 目录={}",
            read_count, skip_count, dir
        )
        .green()
    );
    Ok(())
}

/// 从目录中读取所有 <分区名>.img 或 <分区名>.bin 文件并写回对应分区
pub fn cmd_write_all(
    da: &mut DAXFlash,
    dir: &str,
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::DA分区::GptInfo::parse(&gpt_data)?;

    let mut 写入计数 = 0usize;
    let mut 跳过计数 = 0usize;

    for entry in gpt_info.iter_partitions() {
        // 优先找 .img，其次找 .bin
        let img_path = format!("{}/{}.img", dir, entry.name);
        let bin_path = format!("{}/{}.bin", dir, entry.name);
        let input = if std::path::Path::new(&img_path).exists() {
            img_path
        } else if std::path::Path::new(&bin_path).exists() {
            bin_path
        } else {
            trace!("  跳过 {} (文件不存在)", entry.name);
            跳过计数 += 1;
            continue;
        };

        info!(
            "  [{}/{}] 写入 {} <- {} (0x{:X} 字节)",
            写入计数 + 1,
            gpt_info.partitions().len(),
            entry.name,
            input,
            entry.size
        );
        if let Err(e) = da.写入分区(&entry.name, &input) {
            warn!("  写入 {} 失败: {}", entry.name, e);
            continue;
        }

        if verify {
            let 原始 = std::fs::read(&input).map_err(|e| format!("读取 {} 失败: {}", input, e))?;
            let 验证 = da.readflash_data(entry.start_addr, 原始.len() as u64)?;
            if 原始 != 验证 {
                warn!("  {} 校验失败", entry.name);
            } else {
                info!("  {} 校验通过 ✓", entry.name);
            }
        }
        写入计数 += 1;
    }

    info!(
        "{}",
        format!(
            "分区写入完成: {} 成功, {} 跳过, 目录={}",
            写入计数, 跳过计数, dir
        )
        .green()
    );
    Ok(())
}

/// 打印 scatter 到屏幕并保存文件
pub fn cmd_print_scatter(
    da: &mut DAXFlash,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?;
    let gpt_info = crate::DA分区::GptInfo::parse(gpt_data)?;

    println!();
    println!(
        "{}",
        " Scatter 文件 (SP Flash Tool 格式) ".on_green().black()
    );
    println!();

    let header = crate::DA分区::generate_scatter_header();
    for line in header.lines() {
        println!("{}", line);
    }

    for entry in gpt_info.iter_partitions() {
        println!(
            "{} 0x{:X}",
            entry.name.to_uppercase().green(),
            entry.start_addr
        );
        println!("{{");
        println!("  is_upgradeable: 1");
        println!("  is_download: 1");
        println!("  is_reserved: 0");
        println!("  reserve: 0");
        println!("  operation: UPDATE");
        println!("  partition_size: 0x{:X}", entry.size);
        println!("}}");
        println!();
    }

    let scatter_file = "MT6768_Android_scatter.txt";
    generate_scatter_from_gpt(gpt_data, scatter_file)
        .map_err(|e| format!("scatter 生成失败: {}", e))?;
    info!("{}", format!("Scatter 已保存: {}", scatter_file).green());

    if log_level >= 3 {
        let debug_file = format!(
            "scatter_debug_{}.txt",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        generate_scatter_from_gpt(gpt_data, &debug_file)
            .map_err(|e| format!("调试 scatter 生成失败: {}", e))?;
    }

    Ok(())
}
