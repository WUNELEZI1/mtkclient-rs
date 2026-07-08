//! 轻量级 build.prop 读取器
//!
//! 通过在 system 分区原始数据中搜索 Android 属性 key=value 字符串，
//! 提取 build.prop 信息，无需 ext4 文件系统解析。
//!
//! build.prop 文件内容通常存储在 ext4 文件系统中，但属性值以
//! `ro.product.model=XXX\n` 格式存储。在分区原始数据中，
//! 搜索这些 key 前缀即可定位属性值（可能匹配到多次，
//! 取最后一次出现或第一次出现的有效值）。

use colored::Colorize;
use log::{info, warn};
use std::collections::HashMap;

use crate::da::DAXFlash;

/// 需要提取的关键属性及其显示名称
const BUILDPROP_KEYS: &[(&str, &str)] = &[
    ("ro.product.model", "产品型号"),
    ("ro.product.brand", "品牌"),
    ("ro.product.name", "产品名称"),
    ("ro.product.device", "设备代号"),
    ("ro.product.manufacturer", "制造商"),
    ("ro.build.version.release", "Android 版本"),
    ("ro.build.version.sdk", "SDK 版本"),
    ("ro.build.version.incremental", "构建号"),
    ("ro.build.display.id", "显示版本"),
    ("ro.build.fingerprint", "构建指纹"),
    ("ro.build.type", "构建类型"),
    ("ro.build.tags", "构建标签"),
    ("ro.hardware", "硬件平台"),
    ("ro.board.platform", "SoC 平台"),
    ("ro.bootimage.build.fingerprint", "Boot 镜像指纹"),
    ("ro.system.build.version.release", "System Android 版本"),
    ("ro.vendor.build.version.release", "Vendor Android 版本"),
    ("ro.product.first_api_level", "首次 API 级别"),
    ("ro.treble.enabled", "Treble 启用"),
    ("persist.sys.timezone", "时区"),
];

/// 从分区原始数据中搜索并提取 build.prop 属性
///
/// 在整个数据缓冲区中搜索每个 key 的出现位置，
/// 提取等号后到换行符/回车符/null 之间的 value。
pub fn extract_buildprop(data: &[u8]) -> HashMap<String, String> {
    let mut props = HashMap::new();

    for &(key, _label) in BUILDPROP_KEYS {
        // 搜索 "key=" 模式（注意末尾的等号）
        let search_pattern = format!("{}=", key);
        let pattern_bytes = search_pattern.as_bytes();

        let mut search_start = 0;
        // 取最后一次出现（通常是最新的覆盖值）
        let mut last_value: Option<String> = None;

        while search_start < data.len() {
            if let Some(pos) = memchr::memmem::find(&data[search_start..], pattern_bytes) {
                let abs_pos = search_start + pos + pattern_bytes.len();
                // 提取 value: 从等号后到换行/回车/null
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

/// 格式化输出 build.prop 属性到控制台
pub fn print_buildprop(props: &HashMap<String, String>) {
    let mut found = 0;
    println!();
    println!("  {} {}", "┌─────────────────────────────────────────────────".cyan(), "┐".cyan());
    println!("  {} {}", "│  Android Build Properties".cyan(), "│".cyan());
    println!("  {} {}", "├─────────────────────────────────────────────────".cyan(), "┤".cyan());

    for &(key, label) in BUILDPROP_KEYS {
        if let Some(value) = props.get(key) {
            found += 1;
            let display_label = format!("  {} {:<24}", "│".cyan(), label);
            let display_value = if value.len() > 40 {
                format!("{}...", &value[..37])
            } else {
                value.clone()
            };
            // 右侧补齐到固定宽度
            let padding = 50usize.saturating_sub(label.chars().count());
            let padded = format!("{}{}", display_label, " ".repeat(padding));
            println!("  {}{} {}", padded.cyan(), display_value, "│".cyan());
        }
    }

    if found == 0 {
        println!("  {} {}", "│  (未找到属性数据)".dimmed(), "│".cyan());
    }

    println!("  {} {}", "└─────────────────────────────────────────────────".cyan(), "┘".cyan());
    println!();

    if found > 0 {
        info!("成功提取 {} 个属性", found);
    } else {
        warn!("未找到任何 build.prop 属性（分区可能不是 ext4 格式或数据不完整）");
    }
}

/// buildprop 命令入口
///
/// 用法：
///   mtkclient buildprop              → 从 system 动态分区读取
///   mtkclient buildprop system_a     → 指定分区名
///   mtkclient buildprop <file>      → 从本地镜像文件读取
pub fn cmd_buildprop(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    // 确定数据源
    let data = if args.is_empty() || args[0] == "system" {
        // 从设备读取 system 动态分区（或 system_a）
        info!("从设备读取 system 分区...");
        let part_name = args.first().map(|s| s.as_str()).unwrap_or("system");
        let (addr, size) = da.find_partition_addr(part_name)
            .map_err(|e| format!("查找分区 {} 失败: {}", part_name, e))?;
        info!("分区 {}: 0x{:08X}, {} 字节", part_name, addr, size);

        // 只读前 256MB 足够包含 build.prop（全量读取太慢）
        let read_size = std::cmp::min(size, 256 * 1024 * 1024);
        info!("读取前 {} 字节用于搜索...", read_size);
        da.readflash_data(addr, read_size)
            .map_err(|e| format!("读取失败: {}", e))?
    } else {
        // 从本地文件读取
        let file_path = &args[0];
        info!("从文件读取: {}", file_path);
        std::fs::read(file_path)
            .map_err(|e| format!("读取文件失败: {}", e))?
    };

    info!("在 {} 字节数据中搜索 build.prop 属性...", data.len());
    let props = extract_buildprop(&data);
    print_buildprop(&props);

    Ok(())
}
