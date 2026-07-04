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
                    println!("{}", " eMMC Info (简化) ".on_yellow().black());
                    println!(
                        "  Boot1: {} ({:.2} MB)",
                        format!("0x{:06X}", simple_info.boot1_size).green(),
                        simple_info.boot1_size as f64 / 1_048_576.0
                    );
                    println!(
                        "  Boot2: {} ({:.2} MB)",
                        format!("0x{:06X}", simple_info.boot2_size).green(),
                        simple_info.boot2_size as f64 / 1_048_576.0
                    );
                } else {
                    // 两种方式都失败，记录警告日志
                    warn!("eMMC 信息获取失败 (get_emmc_info 和 get_emmc_info_simple 均返回错误)");
                    println!();
                    println!(
                        "{}",
                        " 警告: 无法获取 eMMC Boot1/Boot2 信息 "
                            .on_red()
                            .white()
                            .bold()
                    );
                    println!(
                        "{}",
                        " 设备可能不支持 0x01010C 命令，或 DA 会话异常。".yellow()
                    );
                    println!(
                        "{}",
                        " 分区表仍将正常显示，但 Boot1/Boot2 行将缺失。".yellow()
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

/// 打印完整 EMMC 信息（英文标签，紧凑格式）
fn print_emmc_info(info: &crate::DA扩展::EmmcInfo) {
    // 格式化字节数为人类可读字符串
    fn fmt_bytes(b: u64) -> String {
        if b >= 1_073_741_824 {
            format!("{:.2} GB", b as f64 / 1_073_741_824.0)
        } else if b >= 1_048_576 {
            format!("{:.2} MB", b as f64 / 1_048_576.0)
        } else if b >= 1024 {
            format!("{:.2} KB", b as f64 / 1024.0)
        } else {
            format!("{} B", b)
        }
    }

    // 每列先格式化定宽字符串，再整体上色，避免 ANSI 破坏对齐
    let mut rows: Vec<(String, String, String)> = Vec::new();
    rows.push((
        format!("{:<10}", "Type"),
        info.emmc_type.clone(),
        String::new(),
    ));
    if info.user_size > 0 {
        rows.push((
            format!("{:<10}", "User Area"),
            format!(
                "{} ({})",
                fmt_bytes(info.user_size),
                format_bytes_comma(info.user_size)
            ),
            String::new(),
        ));
    }
    if info.boot1_size > 0 {
        rows.push((
            format!("{:<10}", "Boot1"),
            format!(
                "{} ({})",
                fmt_bytes(info.boot1_size),
                format_bytes_comma(info.boot1_size)
            ),
            String::new(),
        ));
    }
    if info.boot2_size > 0 {
        rows.push((
            format!("{:<10}", "Boot2"),
            format!(
                "{} ({})",
                fmt_bytes(info.boot2_size),
                format_bytes_comma(info.boot2_size)
            ),
            String::new(),
        ));
    }
    if info.rpmb_size > 0 {
        rows.push((
            format!("{:<10}", "RPMB"),
            format!(
                "{} ({})",
                fmt_bytes(info.rpmb_size),
                format_bytes_comma(info.rpmb_size)
            ),
            String::new(),
        ));
    }
    if info.block_size > 0 {
        rows.push((
            format!("{:<10}", "Block Size"),
            format!("0x{:X} ({} bytes)", info.block_size, info.block_size),
            String::new(),
        ));
    }
    if !info.cid.is_empty() {
        let trimmed: Vec<u8> = info.cid.iter().copied().filter(|&b| b != 0).collect();
        if !trimmed.is_empty() {
            let cid_str: String = trimmed
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            rows.push((format!("{:<10}", "CID"), cid_str, String::new()));
        }
    }

    // 计算最大内容宽度（不包含标签列的空格）
    let max_val_w = rows.iter().map(|(_, v, _)| v.len()).max().unwrap_or(20);
    let total_inner = 10 + 2 + max_val_w; // "Type      " + "  " + value

    println!();
    // 标题行
    let title = " eMMC Info ".to_string();
    let total_w = total_inner.max(title.len() + 2);
    let pad = total_w - title.len();
    let lpad = pad / 2;
    let rpad = pad - lpad;
    println!("+{}+", "-".repeat(total_w));
    println!(
        "|{}{}{}|",
        " ".repeat(lpad),
        title.green().bold(),
        " ".repeat(rpad)
    );
    println!("+{}+", "-".repeat(total_w));

    for (label, value, _extra) in &rows {
        let padded_val = format!("{:<width$}", value, width = max_val_w);
        println!("| {}  {} |", label.green().bold(), padded_val.green());
    }

    println!("+{}+", "-".repeat(total_w));
    println!();
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
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

/// 打印 GPT 表格到控制台（含 eMMC_Boot1/Boot2 显示）
/// 使用 ASCII 表格风格: | 和 - 作为分隔符，列标签中文
fn print_gpt_table(data: &[u8], boot1_size: u64, boot2_size: u64) {
    let gpt_info = match crate::DA分区::GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let partitions = gpt_info.partitions();

    // 列宽定义（地址按 0x%016X = 18 字符，含 0x 前缀）
    const W_IDX: usize = 4; // "01" 等
    const W_NAME: usize = 22; // 分区名
    const W_ADDR: usize = 18; // 0x00000000000000
    const W_SIZE: usize = 10; // "1024.00 MB"
    const W_BYTES: usize = 17; // "1,073,741,824"

    // 构建分隔线: +----+--------+...
    let sep = format!(
        "+-{}-+-{}-+-{}-+-{}-+-{}-+-{}-+",
        "-".repeat(W_IDX),
        "-".repeat(W_NAME),
        "-".repeat(W_ADDR),
        "-".repeat(W_ADDR),
        "-".repeat(W_SIZE),
        "-".repeat(W_BYTES)
    );

    // 表头行（先格式化定宽纯文本，再整体上色）
    let header_plain = format!(
        "| {:<idx$} | {:<name$} | {:>addr$} | {:>addr$} | {:>size$} | {:>bytes$} |",
        "编号",
        "名称",
        "起始地址",
        "结束地址",
        "大小",
        "字节数",
        idx = W_IDX,
        name = W_NAME,
        addr = W_ADDR,
        size = W_SIZE,
        bytes = W_BYTES,
    );

    // 格式化一行为定宽纯文本（不含颜色代码）
    fn fmt_row(idx: &str, name: &str, start: &str, end: &str, size: &str, bytes: &str) -> String {
        format!(
            "| {:<4} | {:<22} | {:>18} | {:>18} | {:>10} | {:>17} |",
            idx, name, start, end, size, bytes
        )
    }

    println!();
    println!("{}", sep);
    println!("{}", header_plain.bright_white().bold());
    println!("{}", sep);

    let mut row = 0usize;

    // eMMC Boot 区域（灰色显示，标记为 eMMC_Boot1 / eMMC_Boot2）
    if boot1_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot1",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot1_size.saturating_sub(1)),
            &format_size(boot1_size),
            &format_bytes_comma(boot1_size),
        );
        println!("{}", line.dimmed());
    }
    if boot2_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot2",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot2_size.saturating_sub(1)),
            &format_size(boot2_size),
            &format_bytes_comma(boot2_size),
        );
        println!("{}", line.dimmed());
    }

    // GPT 分区（交替亮度，先 format 定宽再整体上色）
    for (i, entry) in partitions.iter().enumerate() {
        row += 1;
        let start_addr = entry.start_addr;
        let end_addr = start_addr.saturating_add(entry.size).saturating_sub(1);
        let line = fmt_row(
            &format!("{:02}", row),
            &entry.name,
            &format!("0x{:016X}", start_addr),
            &format!("0x{:016X}", end_addr),
            &format_size(entry.size),
            &format_bytes_comma(entry.size),
        );

        if i % 2 == 0 {
            println!("{}", line);
        } else {
            println!("{}", line.bright_white());
        }
    }

    // 底部分隔线 + 总计
    println!("{}", sep);
    // Total 行横跨整个表格宽度，使用中文标签
    let total_label = format!("| 共 {} 个分区", row);
    // 计算分隔线总宽度
    let sep_len = sep.len();
    let total_text = format!("{:<width$} |", total_label, width = sep_len - 1);
    println!("{}", total_text.green().bold());
    println!("{}", sep);
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
pub fn cmd_read_all(
    da: &mut DAXFlash,
    dir: &str,
    app_config: &AppConfig,
) -> Result<(), Box<dyn std::error::Error>> {
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
