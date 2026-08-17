//! 轻量级 build.prop 读取器
//!
//! 实现策略（按优先级）：
//! 1. ext4 文件系统解析（复用 fs_shell explorer）：精确定位 /system/build.prop
//! 2. 原始数据搜索：读取前 64MB，搜索 key=value 字符串
//! 3. Virtual A/B COW 合并：ext4 解析或原始搜索失败后，尝试 COW 分区合并
//!
//! 支持从设备分区或本地镜像文件读取。

use crate::color::Colorize;
use log::{info, warn};
use std::collections::HashMap;

use crate::da::DAXFlash;
use crate::partition::explorer;

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
    // 系统版本
    ("ro.build.version.release", "Android 版本"),
    ("ro.build.version.os", "系统版本"),
    ("ro.build.version.sdk", "SDK 版本"),
    ("ro.build.version.incremental", "构建号"),
    ("ro.build.version.security_patch", "安全补丁"),
    ("ro.build.display.id", "显示版本"),
    ("ro.build.fingerprint", "构建指纹"),
    ("ro.system.build.fingerprint", "System 构建指纹"),
    ("ro.build.type", "构建类型"),
    ("ro.build.tags", "构建标签"),
    ("ro.build.date", "构建日期"),
    ("ro.system.build.date", "System 构建日期"),
    ("ro.hardware", "硬件平台"),
    ("ro.board.platform", "SoC 平台"),
    ("ro.product.property_source_order", "属性优先级"),
    ("ro.product.first_api_level", "首次 API 级别"),
    // Treble 与安全
    ("ro.treble.enabled", "Treble 启用"),
    ("ro.secure", "安全模式"),
    ("ro.adb.secure", "ADB 安全"),
    ("ro.debuggable", "可调试"),
    ("ro.allow.mock.location", "允许模拟位置"),
    ("security.perf_harden", "安全加固"),
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
            if let Some(pos) = crate::util::memmem_find(&data[search_start..], pattern_bytes) {
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

/// 保存 build.prop 到当前目录
fn save_buildprop_file(data: &[u8]) {
    let output_path = "build.prop";
    match std::fs::write(output_path, data) {
        Ok(_) => info!("build.prop 已保存到当前目录 ({} 字节)", data.len()),
        Err(e) => warn!("保存 build.prop 失败: {}", e),
    }
}

/// 计算字符串的终端显示宽度（中文字符算 2 列）
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if c.is_ascii() {
                1
            } else {
                2 // CJK 字符占 2 列
            }
        })
        .sum()
}

/// 格式化输出 build.prop 属性
pub fn print_buildprop(props: &HashMap<String, String>, source: &str) {
    let mut found = 0;

    // 计算标签列最大宽度
    let max_label_width = BUILDPROP_KEYS
        .iter()
        .filter(|(key, _)| props.contains_key(*key))
        .map(|(_, label)| display_width(label))
        .max()
        .unwrap_or(10);

    println!();
    println!("  {}", "Android Build Properties".cyan().bold());
    println!("  {}", "─".repeat(max_label_width + 40).dimmed());

    for &(key, label) in BUILDPROP_KEYS {
        if let Some(value) = props.get(key) {
            found += 1;
            let label_width = display_width(label);
            let padding = max_label_width.saturating_sub(label_width);
            let display_value = if value.len() > 50 {
                format!("{}...", &value[..47])
            } else {
                value.clone()
            };
            println!(
                "  {} {}{}  {}",
                "│".dimmed(),
                label,
                " ".repeat(padding),
                display_value
            );
        }
    }

    if found == 0 {
        println!("  {}  {}", "│".dimmed(), "(未找到属性数据)".dimmed());
    }

    println!("  {}", "─".repeat(max_label_width + 40).dimmed());
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
///   mtkclient zyb get_build_prop system_b     → 指定逻辑分区名
///   mtkclient zyb get_build_prop <file>       → 从本地镜像文件读取
///
/// 策略（对齐 fs_shell explorer）：
///   1. 从 super 分区读取 LP metadata，构建 extent 映射
///   2. 通过 LP translator 读取目标逻辑分区的 ext4 文件系统
///   3. 解析 build.prop 文件路径并读取内容
pub fn cmd_buildprop(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let target_part = args.first().map(|s| s.as_str()).unwrap_or("system");

    // 本地文件模式
    if std::path::Path::new(target_part).exists() && !target_part.starts_with('-') {
        info!("从文件读取: {}", target_part);
        let file_data = std::fs::read(target_part).map_err(|e| format!("读取文件失败: {}", e))?;

        let props = extract_buildprop(&file_data);
        print_buildprop(&props, &format!("文件 {}", target_part));
        // 本地文件模式不重复保存
        return Ok(());
    }

    // === 确定目标逻辑分区名 ===
    let logical_name = resolve_logical_name(da, target_part)?;
    info!("目标逻辑分区: {}", logical_name);

    // === 策略 1: 通过 LP translator 从 super 读取（对齐 fs_shell） ===
    info!("通过 LP translator 从 super 分区读取...");
    if let Ok((super_addr, _super_size)) = da.find_partition_addr("super") {
        let mut super_read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
            da.readflash_data(super_addr + offset, len)
                .map_err(|e| format!("DA 读取 super 失败: {}", e))
        };

        // 尝试多个常见路径（ext4 内部路径，不含前导 /）
        let prop_paths = [
            "system/build.prop",
            "system/system/build.prop",
            "build.prop",
        ];
        for prop_path in &prop_paths {
            match explorer::read_file_via_lp(&mut super_read_fn, &logical_name, prop_path) {
                Ok(bp_data) => {
                    info!(
                        "LP ext4 读取成功: {}/{} ({} 字节)",
                        logical_name,
                        prop_path,
                        bp_data.len()
                    );
                    let props = extract_buildprop(&bp_data);
                    if !props.is_empty() {
                        // 保存原始 build.prop 到当前目录
                        save_buildprop_file(&bp_data);
                        print_buildprop(
                            &props,
                            &format!("{} (LP ext4: {})", logical_name, prop_path),
                        );
                        return Ok(());
                    }
                    info!("LP ext4 {} 未包含有效属性，尝试下一个路径", prop_path);
                }
                Err(e) => {
                    info!("LP ext4 读取 {}/{} 失败: {}", logical_name, prop_path, e);
                }
            }
        }
    } else {
        warn!("未找到 super 分区，跳过 LP translator 方式");
    }

    // === 策略 2: 回退到旧方式（identity translator + 直接读取） ===
    info!("LP translator 方式失败，回退到直接读取...");
    let (addr, size, actual_name) = find_partition_with_smart(da, target_part)?;
    info!(
        "分区 {} (实际: {}): 0x{:08X}, {} 字节",
        target_part, actual_name, addr, size
    );

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

    let prop_paths_full = [
        "/system/system/build.prop",
        "/system/build.prop",
        "/build.prop",
    ];
    for prop_path in &prop_paths_full {
        match explorer::read_file_by_path(&mut read_fn, size, prop_path) {
            Ok(bp_data) => {
                info!("ext4 读取成功: {} ({} 字节)", prop_path, bp_data.len());
                let props = extract_buildprop(&bp_data);
                if !props.is_empty() {
                    save_buildprop_file(&bp_data);
                    print_buildprop(&props, &format!("{} (ext4: {})", actual_name, prop_path));
                    return Ok(());
                }
            }
            Err(_) => {}
        }
    }

    // === 策略 3: 原始数据搜索 ===
    info!("ext4 解析失败，fallback 到原始搜索...");
    let read_size = std::cmp::min(size, 64 * 1024 * 1024);
    let base_data = da
        .readflash_data(addr, read_size)
        .map_err(|e| format!("读取失败: {}", e))?;

    let props = extract_buildprop(&base_data);
    if !props.is_empty() {
        // 从原始数据中提取纯文本 build.prop 内容
        save_buildprop_file(&base_data);
    }
    print_buildprop(&props, &actual_name);
    Ok(())
}

/// 解析逻辑分区名：system → system_b（根据当前槽位）
fn resolve_logical_name(
    da: &mut DAXFlash,
    target: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // 如果已经是完整的分区名（如 system_b），直接使用
    if target.contains('_') {
        return Ok(target.to_string());
    }

    // 从 misc 分区读取当前活动槽位
    let slot = read_current_slot(da);
    let slot_suffix = if slot == 0 { "_a" } else { "_b" };
    Ok(format!("{}{}", target, slot_suffix))
}

/// 从 misc 分区的 bootctrl 结构读取当前活动槽位
fn read_current_slot(da: &mut DAXFlash) -> u32 {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let misc_data = match da.readflash_data(misc_addr, std::cmp::min(misc_size, 0x3000)) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    if misc_data.len() < 0x200C {
        return 0;
    }
    let magic = u32::from_le_bytes(misc_data[0x2000..0x2004].try_into().unwrap_or([0; 4]));
    if magic == 0x424F4F54 {
        u32::from_le_bytes(misc_data[0x2008..0x200C].try_into().unwrap_or([0; 4]))
    } else {
        0
    }
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
