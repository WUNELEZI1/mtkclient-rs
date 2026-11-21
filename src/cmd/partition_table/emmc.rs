//! eMMC capacity formatting helpers (reused by GPT table rendering).

/// Format a byte count as a human-readable string (KB/MB/GB, 2 decimals)
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

/// Format a byte count with thousands separators
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
