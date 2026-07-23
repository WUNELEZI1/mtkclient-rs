//! 轻量级 build.prop 读取器
//!
//! 实现策略（按优先级）：
//! 1. ext4 文件系统解析（复用 fs_shell explorer）：精确定位 /system/build.prop
//! 2. 原始数据搜索：读取前 64MB，搜索 key=value 字符串
//! 3. Virtual A/B COW 合并：ext4 解析或原始搜索失败后，尝试 COW 分区合并
//!
//! 支持从设备分区或本地镜像文件读取。

use colored::Colorize;
use log::{info, warn};
use std::collections::HashMap;

use crate::da::DAXFlash;
use crate::partition::{cow, explorer};

/// 需要提取的关键属性及其显示名称
const BUILDPROP_KEYS: &[(&str, &str)] = &[
    // Treble system 属性（优先，Android 10+ system 分区标准）
    ("ro.product.system.model", "产品型号"),
    ("ro.product.system.brand", "品牌"),
    ("ro.product.system.name", "产品名称"),
    ("ro.product.system.device", "设备代号"),
    ("ro.product.system.manufacturer", "制造商"),
    // 通用属性（运行时合并值）
    ("ro.product.model", "产品型号(合并)"),
    ("ro.product.brand", "品牌(合并)"),
    ("ro.product.name", "产品名称(合并)"),
    ("ro.product.device", "设备代号(合并)"),
    ("ro.product.manufacturer", "制造商(合并)"),
    // build 属性
    ("ro.build.version.release", "Android 版本"),
    ("ro.build.version.sdk", "SDK 版本"),
    ("ro.build.version.incremental", "构建号"),
    ("ro.build.version.security_patch", "安全补丁"),
    ("ro.build.display.id", "显示版本"),
    ("ro.build.fingerprint", "构建指纹"),
    ("ro.system.build.fingerprint", "System 构建指纹"),
    ("ro.build.type", "构建类型"),
    ("ro.build.tags", "构建标签"),
    ("ro.build.date", "构建日期"),
    ("ro.hardware", "硬件平台"),
    ("ro.board.platform", "SoC 平台"),
    ("ro.product.property_source_order", "属性优先级"),
    ("ro.product.first_api_level", "首次 API 级别"),
];

/// 从数据中提取 build.prop 属性
pub fn extract_buildprop(data: &[u8]) -> HashMap<String, String> {
    let mut props = HashMap::new();

    for &(key, _label) in BUILDPROP_KEYS {
        let search_pattern = format!("{}=", key);
        let pattern_bytes = search_pattern.as_bytes();

        let mut search_start = 0;
        let mut last_value: Option<String> = None;

        while search_start < data.len() {
            if let Some(pos) = memchr::memmem::find(&data[search_start..], pattern_bytes) {
                let abs_pos = search_start + pos + pattern_bytes.len();
                let value_start = abs_pos;
                let value_end = data[value_start..]
                    .iter()
                    .position(|&c| c == b'\n' || c == b'\r' || c == 0)
                    .map(|p| value_start + p)
                    .unwrap_or(data.len().min(value_start + 256));

                if value_end > value_start {
                    let value = String::from_utf8_lossy(&data[value_start..value_end])
                        .trim()
                        .to_string();
                    if !value.is_empty() {
                        last_value = Some(value);
                    }
                }
                search_start = abs_pos;
            } else {
                break;
            }
        }

        if let Some(value) = last_value {
            props.insert(key.to_string(), value);
        }
    }

    props
}

/// 格式化输出 build.prop 属性
pub fn print_buildprop(props: &HashMap<String, String>, source: &str) {
    let mut found = 0;
    println!();
    println!(
        "  {} {}",
        "┌─────────────────────────────────────────────────".cyan(),
        "┐".cyan()
    );
    println!("  {} {}", "│  Android Build Properties".cyan(), "│".cyan());
    println!(
        "  {} {}",
        "├─────────────────────────────────────────────────".cyan(),
        "┤".cyan()
    );

    for &(key, label) in BUILDPROP_KEYS {
        if let Some(value) = props.get(key) {
            found += 1;
            let display_label = format!("  {} {:<24}", "│".cyan(), label);
            let display_value = if value.len() > 40 {
                format!("{}...", &value[..37])
            } else {
                value.clone()
            };
            let padding = 50usize.saturating_sub(label.chars().count());
            let padded = format!("{}{}", display_label, " ".repeat(padding));
            println!("  {}{} {}", padded.cyan(), display_value, "│".cyan());
        }
    }

    if found == 0 {
        println!("  {} {}", "│  (未找到属性数据)".dimmed(), "│".cyan());
    }

    println!(
        "  {} {}",
        "└─────────────────────────────────────────────────".cyan(),
        "┘".cyan()
    );
    println!();

    if found > 0 {
        info!("成功提取 {} 个属性（数据来源: {}）", found, source);
    } else {
        warn!("未找到任何 build.prop 属性（数据来源: {}）", source);
        warn!("可能原因: 分区不是 ext4 格式、数据不完整或属性位置超出搜索范围");
    }
}

/// buildprop 命令入口
///
/// 用法:
///   mtkclient zyb get_build_prop              → 从 system 动态分区读取
///   mtkclient zyb get_build_prop system_a     → 指定分区名
///   mtkclient zyb get_build_prop <file>       → 从本地镜像文件读取
pub fn cmd_buildprop(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let target_part = args.first().map(|s| s.as_str()).unwrap_or("system");

    // 本地文件模式
    if std::path::Path::new(target_part).exists() && !target_part.starts_with('-') {
        info!("从文件读取: {}", target_part);
        let file_data = std::fs::read(target_part).map_err(|e| format!("读取文件失败: {}", e))?;

        let props = extract_buildprop(&file_data);
        print_buildprop(&props, &format!("文件 {}", target_part));
        return Ok(());
    }

    // === 从设备读取分区 ===
    info!("从设备读取 {} 分区...", target_part);

    let (addr, size, actual_name) = find_partition_with_smart(da, target_part)?;
    info!(
        "分区 {} (实际: {}): 0x{:08X}, {} 字节",
        target_part, actual_name, addr, size
    );

    // === 策略 1: 优先使用 ext4 文件系统解析（复用 fs_shell explorer） ===
    info!("尝试 ext4 文件系统解析 (fs_shell)...");
    let mut read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
        if offset + len > size {
            return Err(format!(
                "读取超出分区范围: offset=0x{:X}, len={}, size={}",
                offset, len, size
            ));
        }
        da.readflash_data(addr + offset, len)
            .map_err(|e| format!("DA 读取失败: {}", e))
    };

    // 尝试多个常见路径：/system/build.prop 是 Treble 设备标准路径
    let prop_paths = ["/system/build.prop", "/build.prop", "build.prop"];
    for prop_path in &prop_paths {
        match explorer::read_file_by_path(&mut read_fn, size, prop_path) {
            Ok(bp_data) => {
                info!("ext4 解析成功，从 {} 读取 {} 字节", prop_path, bp_data.len());
                let props = extract_buildprop(&bp_data);
                if !props.is_empty() {
                    print_buildprop(&props, &format!("{} (ext4: {})", actual_name, prop_path));
                    return Ok(());
                }
                info!("ext4 解析 {} 未包含有效 build.prop 属性，尝试下一个路径", prop_path);
            }
            Err(_) => {
                // 继续尝试下一个路径
            }
        }
    }
    info!("ext4 解析未找到 build.prop，fallback 到原始搜索");

    // === 策略 2: 原始数据搜索（前 64MB）===
    let read_size = std::cmp::min(size, 64 * 1024 * 1024);
    info!("读取前 {} 字节用于搜索...", read_size);
    let base_data = da
        .readflash_data(addr, read_size)
        .map_err(|e| format!("读取失败: {}", e))?;

    info!("在 {} 字节数据中搜索 build.prop 属性...", base_data.len());
    let mut props = extract_buildprop(&base_data);

    // 诊断信息
    if props.is_empty() {
        let preview: Vec<String> = base_data[..std::cmp::min(32, base_data.len())]
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect();
        info!(
            "base 数据前 {} 字节: {} | 非零字节数: {}",
            preview.len(),
            preview.join(" "),
            base_data.iter().filter(|&&b| b != 0).count()
        );

        // === 策略 3: Virtual A/B COW 合并 ===
        let has_dynamic = da
            .super_metadata
            .as_ref()
            .map(|m| !m.partitions.is_empty())
            .unwrap_or(false);
        if has_dynamic {
            info!("存在动态分区，尝试 Virtual A/B COW 合并...");
            match try_cow_merge(da, &actual_name, size, Some(&base_data)) {
                Ok((cow_source, merged_data)) => {
                    info!(
                        "COW 合并成功，在 {} 字节合并数据中搜索属性...",
                        merged_data.len()
                    );
                    let cow_props = extract_buildprop(&merged_data);
                    if !cow_props.is_empty() {
                        props = cow_props;
                        print_buildprop(&props, &cow_source);
                        return Ok(());
                    } else {
                        warn!("COW 合并数据中仍未找到属性");
                        print_buildprop(&props, &format!("{} (COW 合并后)", cow_source));
                        return Ok(());
                    }
                }
                Err(e) => {
                    warn!("COW 合并失败: {}", e);
                }
            }
        }
    }

    print_buildprop(&props, &actual_name);
    Ok(())
}

/// 尝试 Virtual A/B COW 合并（metadata-only 模式）
///
/// 不再读取整个 COW 分区（可能 2GB+），只读取 metadata areas（通常几百 KB），
/// 如果 exceptions=0 直接返回错误，避免浪费时间和磁盘空间。
fn try_cow_merge(
    da: &mut DAXFlash,
    partition_name: &str,
    base_size: u64,
    base_data: Option<&[u8]>,
) -> Result<(String, Vec<u8>), Box<dyn std::error::Error>> {
    let cow_name = format!("{}-cow", partition_name);
    info!("查找 COW 分区: {}", cow_name);

    let (cow_addr, cow_size, cow_source) = find_cow_partition(da, &cow_name)?;

    info!(
        "COW 分区 {}: 地址 0x{:08X}, 大小 {} 字节",
        cow_source, cow_addr, cow_size
    );

    // 只读取 metadata areas，不读取整个 COW 分区
    let mut cow_read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
        da.readflash_data(cow_addr + offset, len)
            .map_err(|e| format!("COW 读取失败: {}", e))
    };

    let (chunk_size_sectors, exception_table) =
        cow::snap_build_exception_table(&mut cow_read_fn, cow_size)
            .map_err(|e| format!("读取 COW exception table 失败: {}", e))?;

    if exception_table.is_empty() {
        return Err("COW exception table 为空（0 个 exceptions），无需合并".into());
    }

    info!(
        "COW chunk_size={} sectors, exceptions={}, 开始定向合并...",
        chunk_size_sectors,
        exception_table.len()
    );

    let chunk_bytes = (chunk_size_sectors as u64) * 512;
    let base = match base_data {
        Some(data) => data.to_vec(),
        None => {
            return Err("COW 合并需要提供 base_data".into());
        }
    };

    // 定向合并：只应用有 exception 的 chunk
    let mut merged = base.clone();
    let mut max_offset = 0u64;

    for (&old_chunk, &new_chunk) in &exception_table {
        let cow_offset = new_chunk * chunk_bytes;
        let base_offset = old_chunk * chunk_bytes;
        let read_len = std::cmp::min(chunk_bytes, base_size.saturating_sub(base_offset));

        if read_len == 0 {
            continue;
        }

        let chunk_data = da
            .readflash_data(cow_addr + cow_offset, read_len)
            .map_err(|e| format!("COW 数据读取失败: {}", e))?;

        let base_start = base_offset as usize;
        let base_end = std::cmp::min(base_start + chunk_data.len(), merged.len());

        if base_start < merged.len() {
            merged[base_start..base_end].copy_from_slice(&chunk_data[..base_end - base_start]);
            max_offset = max_offset.max(base_end as u64);
        }
    }

    info!(
        "COW 定向合并完成: {} 个 exception 已应用, 有效数据范围: 0..0x{:X}",
        exception_table.len(),
        max_offset
    );

    let effective_data = if (max_offset as usize) < merged.len() {
        merged[..max_offset as usize].to_vec()
    } else {
        merged
    };

    Ok((format!("{} (COW 合并)", cow_source), effective_data))
}

/// 查找 COW 分区地址
fn find_cow_partition(
    da: &mut DAXFlash,
    cow_name: &str,
) -> Result<(u64, u64, String), Box<dyn std::error::Error>> {
    if let Ok((addr, size)) = da.find_partition_addr(cow_name) {
        return Ok((addr, size, cow_name.to_string()));
    }

    da.ensure_super_metadata()
        .map_err(|e| format!("加载 super 元数据失败: {}", e))?;

    if let Some(ref meta) = da.super_metadata {
        if let Some((offset, size)) = meta.find_partition(cow_name) {
            let (super_addr, _) = da.find_partition_addr("super")?;
            info!(
                "从 super 动态分区中找到 {}: 偏移 0x{:08X}, 大小 0x{:08X}",
                cow_name, offset, size
            );
            return Ok((super_addr + offset, size, cow_name.to_string()));
        }
    }

    Err(format!("未找到 COW 分区 {}", cow_name).into())
}

/// 查找分区地址
fn find_partition_with_smart(
    da: &mut DAXFlash,
    name: &str,
) -> Result<(u64, u64, String), Box<dyn std::error::Error>> {
    if let Ok((addr, size)) = da.find_partition_addr(name) {
        return Ok((addr, size, name.to_string()));
    }

    for suffix in &["_a", "_b"] {
        let slot_name = format!("{}{}", name, suffix);
        if let Ok((addr, size)) = da.find_partition_addr(&slot_name) {
            return Ok((addr, size, slot_name));
        }
    }

    da.ensure_super_metadata()
        .map_err(|e| format!("加载 super 元数据失败: {}", e))?;

    if let Some(ref meta) = da.super_metadata {
        if let Some((slot_name, offset, size)) = meta.find_partition_smart(name) {
            if let Ok((super_addr, _)) = da.find_partition_addr("super") {
                return Ok((super_addr + offset, size, slot_name));
            }
        }
    }

    Err(format!("未找到分区 {}（尝试了精确匹配、A/B 槽位和动态分区）", name).into())
}
