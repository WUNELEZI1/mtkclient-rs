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

use crate::da::DAXFlash;
use crate::partition::generate_scatter_from_gpt;
use crate::system::config::AppConfig;

mod emmc;
mod table;

/// 打印 GPT 分区表
pub fn cmd_printgpt(da: &mut DAXFlash, log_level: u8) {
    match da.read_gpt() {
        Ok(_) => {
            let mut boot1_size: u64 = 0;
            let mut boot2_size: u64 = 0;

            // 尝试加载 super 动态分区元数据（用于 printgpt 展示 logical partition）
            if da.super_metadata.is_none() {
                if let Ok((super_addr, super_size)) = da.find_partition_addr("super") {
                    let meta_size = std::cmp::min(super_size, 1024 * 1024);
                    match da.readflash_data(super_addr, meta_size) {
                        Ok(super_data) => {
                            match crate::partition::lp::SuperMetadata::parse(&super_data) {
                                Ok(meta) => {
                                    info!("Super 动态分区: {} 个 logical partition", meta.partitions.len());
                                    da.super_metadata = Some(meta);
                                }
                                Err(_) => {}
                            }
                        }
                        Err(_) => {}
                    }
                }
            }

            // 输出完整 EMMC 信息
            if let Ok(emmc_info) = da.get_emmc_info() {
                boot1_size = emmc_info.boot1_size;
                boot2_size = emmc_info.boot2_size;
                emmc::print_emmc_info(&emmc_info);
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
                table::print_gpt_table(data, boot1_size, boot2_size, da.super_metadata.as_ref());
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
    DAXFlash::save_gpt_cache_file(&output, gpt_data).map_err(|e| format!("写入失败: {}", e))?;
    info!(
        "{}",
        format!("GPT 已保存: {} ({} 字节)", output, gpt_data.len()).green()
    );

    if log_level >= 2 {
        table::print_gpt_table(gpt_data, 0, 0, None);
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
    let gpt_info = crate::partition::GptInfo::parse(&gpt_data)?;
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
    let gpt_info = crate::partition::GptInfo::parse(&gpt_data)?;

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
