//! eMMC 容量格式化助手（供 GPT 表格渲染复用）。

/// 格式化字节数为人类可读字符串 (KB/MB/GB, 保留 2 位小数)
pub fn format_size(bytes: u64) -> String {
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
pub fn format_bytes_comma(bytes: u64) -> String {
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
