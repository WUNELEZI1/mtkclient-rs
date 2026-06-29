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

use crate::DA分区::{generate_scatter_from_gpt, generate_scatter_shoujixia};
use crate::DA扩展::DAXFlash;

/// 打印 GPT 分区表
pub fn cmd_printgpt(da: &mut DAXFlash, log_level: u8) {
    match da.read_gpt() {
        Ok(_) => {
            // 输出完整 EMMC 信息
            if let Ok(emmc_info) = da.get_emmc_info() {
                print_emmc_info(&emmc_info);
            } else {
                // 失败时降级到读 boot1/boot2
                if let Ok(simple_info) = da.get_emmc_info_simple() {
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
                print_gpt_table(data);
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
                // 同时输出刷机匣格式 scatter
                let shoujixia_file = "scatter_shoujixia.txt";
                if let Err(e) = generate_scatter_shoujixia(data, shoujixia_file, "MT6768") {
                    info!("  Warning: 刷机匣格式 scatter 生成失败: {}", e);
                } else {
                    info!("  {} 已生成", shoujixia_file);
                }
            }
        }
        Err(e) => error!("{}", format!("GPT 读取失败: {}", e).red()),
    }
}

/// 打印完整 EMMC 信息
fn print_emmc_info(info: &crate::DA扩展::EmmcInfo) {
    println!();
    println!("{}", " EMMC 信息 ".on_green().black());
    println!(
        "  存储类型:         {}",
        info.emmc_type.green()
    );
    if info.user_size > 0 {
        println!(
            "  用户区大小:       {}  {} GB",
            format!("0x{:X}", info.user_size).green(),
            format!("({} GB / {} MB)", info.user_size_gb(), info.user_size_mb()).dimmed()
        );
    }
    if info.boot1_size > 0 {
        println!(
            "  Boot1 大小:       {}  {} MB",
            format!("0x{:06X}", info.boot1_size).green(),
            format!("({} MB)", info.boot1_size_mb()).dimmed()
        );
    }
    if info.boot2_size > 0 {
        println!(
            "  Boot2 大小:       {}  {} MB",
            format!("0x{:06X}", info.boot2_size).green(),
            format!("({} MB)", info.boot2_size_mb()).dimmed()
        );
    }
    if info.rpmb_size > 0 {
        println!(
            "  RPMB 大小:        {}  {} KB",
            format!("0x{:06X}", info.rpmb_size).green(),
            format!("({} KB)", info.rpmb_size / 1024).dimmed()
        );
    }
    if info.block_size > 0 {
        println!(
            "  块大小:           {}",
            format!("0x{:X} ({} 字节)", info.block_size, info.block_size).green()
        );
    }
    if !info.cid.is_empty() {
        // CID 段去除末尾 0 字节再展示
        let trimmed: Vec<u8> = info.cid.iter().copied().filter(|&b| b != 0).collect();
        if !trimmed.is_empty() {
            let cid_str: String = trimmed
                .iter()
                .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
                .collect();
            println!("  CID:              {}", cid_str.green());
            println!("  CID (HEX):        {}", format!("{:02X?}", info.cid).dimmed());
        }
    }
}

/// 打印 GPT 表格到控制台
fn print_gpt_table(data: &[u8]) {
    let gpt_info = match crate::DA分区::GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let revision = gpt_info.revision;
    let num_part_entries = gpt_info.num_part_entries;
    let part_entry_size = gpt_info.part_entry_size;

    println!();
    println!("{}", " GPT 分区表 ".on_green().black());
    println!("  修订版本:     {}", format!("0x{:08X}", revision).green());
    println!("  头部大小:     {} 字节", gpt_info.header_size);
    println!(
        "  分区数量:     {}",
        format!("{}", num_part_entries).green()
    );
    println!("  分区项大小:   {} 字节", part_entry_size);

    println!();
    println!(
        "{:<4} {:<20} {:<20} {:<20}",
        "序号".cyan(),
        "分区名称".cyan(),
        "起始地址".cyan(),
        "大小".cyan()
    );
    println!("{}", "─".repeat(66).dimmed());

    let partitions = gpt_info.partitions();
    for (count, entry) in partitions.iter().enumerate() {
        println!(
            "{:<4} {:<20} {:<20} {:<20}",
            format!("#{}", count + 1).dimmed(),
            entry.name.green(),
            format!("0x{:014X}", entry.start_addr).yellow(),
            format!("0x{:014X}", entry.size).yellow(),
        );
    }

    println!("{}", "─".repeat(66).dimmed());
    println!("  共 {} 个分区", format!("{}", partitions.len()).green());
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
        print_gpt_table(gpt_data);
    }

    Ok(())
}

/// 读取全部分区到目录
pub fn cmd_read_all(da: &mut DAXFlash, dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::DA分区::GptInfo::parse(&gpt_data)?;

    for entry in gpt_info.iter_partitions() {
        let output = format!("{}/{}.img", dir, entry.name);
        info!(
            "  读取分区 {} (0x{:X} @ 0x{:X})",
            entry.name, entry.size, entry.start_addr
        );
        let data = da
            .readflash_data(entry.start_addr, entry.size)
            .map_err(|e| format!("读取 {} 失败: {}", entry.name, e))?;
        std::fs::write(&output, &data).map_err(|e| format!("写入失败: {}", e))?;
        info!("{}", format!("  {} -> {}", entry.name, output).green());
    }

    info!("{}", format!("全部分区已读取到: {}", dir).green());
    Ok(())
}

/// 从目录中读取所有 <分区名>.img 文件并写回对应分区
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
        let input = format!("{}/{}.img", dir, entry.name);
        if !std::path::Path::new(&input).exists() {
            trace!("  跳过 {} (文件不存在: {})", entry.name, input);
            跳过计数 += 1;
            continue;
        }

        info!(
            "  写入分区 {} <- {} (0x{:X} 字节)",
            entry.name, input, entry.size
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
            "全部分区写入完成: {} 成功, {} 跳过, 目录={}",
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

    let shoujixia_file = "scatter_shoujixia.txt";
    if let Err(e) = generate_scatter_shoujixia(gpt_data, shoujixia_file, "MT6768") {
        info!("Warning: 刷机匣格式 scatter 生成失败: {}", e);
    } else {
        info!(
            "{}",
            format!("刷机匣格式 scatter 已保存: {}", shoujixia_file).green()
        );
    }

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
