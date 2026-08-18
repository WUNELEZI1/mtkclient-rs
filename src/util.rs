//! 通用工具函数
//!
//! 统一存放项目中重复使用的工具函数，避免各模块各自定义。

/// 将字节数组格式化为大写 HEX 字符串，字节间用空格分隔
///
/// 示例: `[0xAB, 0xCD]` → `"AB CD"`
pub fn hex_str(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 将 HEX 字符串解析为字节数组
///
/// 支持以下格式:
/// - `"AABBCCDD"` (无分隔符)
/// - `"AA BB CC DD"` (空格分隔)
/// - `"AA:BB:CC:DD"` (冒号分隔)
/// - `"0xAABBCCDD"` (0x 前缀)
///
/// 返回解析错误信息
pub fn parse_hex(hex_str: &str) -> Result<Vec<u8>, String> {
    // 移除 0x 前缀
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    // 移除所有分隔符（空格、冒号等）
    let s: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();

    if s.is_empty() {
        return Err("HEX 字符串为空".to_string());
    }
    if s.len() % 2 != 0 {
        return Err(format!(
            "HEX 字符串长度不是偶数: {} ({} 字符)",
            hex_str,
            s.len()
        ));
    }

    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("HEX 解析失败 @ 位置 {}: {}", i, e))
        })
        .collect()
}

/// 生成 hex dump 格式输出（类似 xxd/xxd 风格）
///
/// 输出格式: `ADDR  XX XX XX XX XX XX XX XX  XX XX XX XX XX XX XX XX  |ASCII...........|`
pub fn hex_dump(data: &[u8], base_addr: u64) -> String {
    let mut lines = Vec::new();
    for chunk in data.chunks(16) {
        let offset = base_addr + (data.len() - lines.len() * 16 - chunk.len()) as u64;
        let hex_part: String = chunk
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if i == 8 {
                    format!(" {:02X}", b)
                } else {
                    format!("{:02X}", b)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        // 补齐空格
        let hex_padded = if chunk.len() < 16 {
            format!("{:<47}", hex_part)
        } else {
            hex_part
        };

        let ascii_part: String = chunk
            .iter()
            .map(|&b| {
                if b >= 0x20 && b < 0x7F {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();

        lines.push(format!(
            "  {:08X}  {}  |{}|",
            offset, hex_padded, ascii_part
        ));
    }
    lines.join("\n")
}

/// 在 `haystack` 中查找子串 `needle` 首次出现的位置（相对 `haystack` 起点的偏移）。
///
/// 自研替代 `memchr::memmem::find`，用于 build.prop 属性提取等小规模字节序列搜索。
/// 采用简单线性扫描，对单分区属性扫描（KB~MB 级）性能足够；如需大文本高频搜索
/// 可后续引入 SIMD 优化，但本项目场景下无必要。返回 `None` 表示未找到。
pub fn memmem_find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 计算字符串的终端显示宽度（字符单元格数）。
///
/// 自研替代 `unicode_width::UnicodeWidthStr::width`，用于分区表的中文双宽对齐。
/// 规则：ASCII 及半数常见符号宽度 1；CJK 统一表意文字、全角标点、Hangul 音节块等
/// 东亚全角字符宽度 2；其余未覆盖的非 ASCII 字符保守按宽度 2 处理（避免中文标签
/// 对齐错位——宁可略宽也不致重叠）。仅影响显示对齐，不影响任何功能逻辑。
pub fn display_width(input: &str) -> usize {
    input
        .chars()
        .map(|c| if is_fullwidth(c) { 2 } else { 1 })
        .sum()
}

/// 判断字符是否为东亚全宽（显示宽度 2）。
///
/// 仅对列出的 Unicode 全宽区块返回 `true`：Hangul Jamo、CJK 部首/康熙部首、
/// 假名、CJK 统一表意文字（含扩展 A/B+）、Hangul 音节、全角 ASCII 变体、
/// CJK 兼容字形与兼容形式、符号与 Pictograph（emoji）等。
/// 其余字符（含 ASCII，以及阿拉伯文/希腊文/西里尔文等 sub-0x1100 非 ASCII）
/// 一律视为宽度 1。该实现面向以 CJK 为主的分区表对齐场景，对未覆盖的非 ASCII
/// 字符宁可估窄（宽度 1）也不过度占位；仅影响显示对齐，不影响任何功能逻辑。
fn is_fullwidth(c: char) -> bool {
    let code = c as u32;
    // ASCII 及 C0/C1 控制字符：窄
    if code < 0x1100 {
        return false;
    }
    matches!(
        code,
        0x1100..=0x115F       // Hangul Jamo
        | 0x2E80..=0x303E     // CJK 部首补充 + 康熙部首 + 表意描述符
        | 0x3041..=0x33FF     // Hiragana/Katakana + 半/全角形 + 谚文兼容 Jamo
        | 0x3400..=0x4DBF     // CJK 扩展 A
        | 0x4E00..=0x9FFF     // CJK 统一表意文字
        | 0xA000..=0xA4CF     // 彝文 + 谚文音节
        | 0xAC00..=0xD7A3     // Hangul 音节
        | 0xF900..=0xFAFF     // CJK 兼容象形
        | 0xFE30..=0xFE4F     // CJK 兼容形式
        | 0xFF00..=0xFF60     // 全角 ASCII
        | 0xFFE0..=0xFFE6     // 全角符号
        | 0x1F300..=0x1FAFF   // 符号与 Pictograph（emoji 等，保守全宽）
        | 0x20000..=0x3FFFD   // CJK 扩展 B+ 及超大字符集
    )
}
