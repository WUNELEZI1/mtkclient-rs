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
