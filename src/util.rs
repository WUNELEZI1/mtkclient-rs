//! Shared utility functions
//!
//! Common helpers used across the project live here, so no module defines its own copy.

/// Parse a u64 from hex, accepting an optional `0x`/`0X` prefix and surrounding whitespace.
///
/// MediaTek LK's `partition-size:<part>` variables often return hex without the `0x`
/// prefix (e.g. `1a638f8000`); `u64::from_str_radix(s, 16)` yields the correct hex value
/// for plain digit strings too, so there is no decimal fallback — non-hex input should error.
pub fn parse_u64_hex(s: &str) -> Result<u64, std::num::ParseIntError> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16);
    }
    u64::from_str_radix(s, 16)
}

/// Strip a single leading `0x` or `0X` prefix, if present; otherwise return `s` unchanged.
///
/// Whitespace and underscore normalization is deliberately left to callers so each parse
/// site keeps its original trim / validation order. A caller that needs to know whether a
/// prefix was present can compare the returned length with the input length.
pub fn strip_hex_prefix(s: &str) -> &str {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
}

/// Format a byte array as an uppercase HEX string separated by spaces
///
/// Example: `[0xAB, 0xCD]` → `"AB CD"`
pub fn hex_str(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a HEX string into a byte array
///
/// Supported formats:
/// - `"AABBCCDD"` (no separator)
/// - `"AA BB CC DD"` (space-separated)
/// - `"AA:BB:CC:DD"` (colon-separated)
/// - `"0xAABBCCDD"` (0x prefix)
///
/// Returns a parse error message
pub fn parse_hex(hex_str: &str) -> Result<Vec<u8>, String> {
    // Strip the 0x prefix
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    // Strip all separators (spaces, colons, ...)
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

/// Produce hex dump output (xxd-like style)
///
/// Output format: `ADDR  XX XX XX XX XX XX XX XX  XX XX XX XX XX XX XX XX  |ASCII...........|`
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

        // Pad with spaces
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

/// Find the first occurrence of `needle` in `haystack` (offset relative to the start of `haystack`).
///
/// In-house replacement for `memchr::memmem::find`, used for small byte-sequence searches such as build.prop extraction.
/// A simple linear scan is fast enough for per-partition property scans (KB~MB); SIMD could be
/// added later for large high-frequency searches, but it is unnecessary here. `None` means not found.
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

/// Compute the terminal display width of a string (in character cells).
///
/// In-house replacement for `unicode_width::UnicodeWidthStr::width`, used to align double-width CJK in partition tables.
/// Rules: ASCII and half-width common symbols are 1; East Asian full-width characters such as CJK unified
/// ideographs, full-width punctuation and Hangul syllable blocks are 2; other uncovered non-ASCII characters
/// conservatively count as 2 (so CJK labels never misalign — slightly too wide beats overlapping). Display only.
pub fn display_width(input: &str) -> usize {
    input
        .chars()
        .map(|c| if is_fullwidth(c) { 2 } else { 1 })
        .sum()
}

/// Whether a character is East Asian full-width (display width 2).
///
/// Returns `true` only for the listed Unicode full-width blocks: Hangul Jamo, CJK radicals / Kangxi radicals,
/// kana, CJK unified ideographs (including extensions A/B+), Hangul syllables, full-width ASCII variants,
/// CJK compatibility ideographs and forms, symbols and pictographs (emoji), etc.
/// Everything else (ASCII, and sub-0x1100 non-ASCII such as Arabic/Greek/Cyrillic) counts as width 1.
/// This targets CJK-heavy partition-table alignment: uncovered non-ASCII characters are estimated narrow
/// (width 1) rather than over-allocating space. Display alignment only; no functional impact.
fn is_fullwidth(c: char) -> bool {
    let code = c as u32;
    // ASCII and C0/C1 control characters: narrow
    if code < 0x1100 {
        return false;
    }
    matches!(
        code,
        0x1100..=0x115F       // Hangul Jamo
        | 0x2E80..=0x303E     // CJK radicals supplement + Kangxi radicals + ideographic description characters
        | 0x3041..=0x33FF     // Hiragana/Katakana + half/full-width forms + Hangul compatibility Jamo
        | 0x3400..=0x4DBF     // CJK Extension A
        | 0x4E00..=0x9FFF     // CJK Unified Ideographs
        | 0xA000..=0xA4CF     // Yi + Hangul syllables
        | 0xAC00..=0xD7A3     // Hangul syllables
        | 0xF900..=0xFAFF     // CJK compatibility ideographs
        | 0xFE30..=0xFE4F     // CJK compatibility forms
        | 0xFF00..=0xFF60     // Full-width ASCII
        | 0xFFE0..=0xFFE6     // Full-width symbols
        | 0x1F300..=0x1FAFF   // Symbols and pictographs (emoji, etc.; conservatively full-width)
        | 0x20000..=0x3FFFD   // CJK Extension B+ and supplementary ideographic planes
    )
}

/// Current UTC time (ISO 8601, second precision), used for audit logging.
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // No third-party crates; compute the civil date by hand (Howard Hinnant's algorithm)
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}

/// CRC32 (IEEE 802.3, polynomial 0xEDB8320) lookup table, generated at compile time.
pub(crate) const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Shared CRC32 lookup table for both one-shot and incremental hashing.
pub(crate) static CRC_TABLE: [u32; 256] = make_crc_table();

/// One-shot CRC32 (IEEE 802.3) calculation.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn iso8601_shape() {
        let s = now_iso8601();
        assert_eq!(s.len(), 20, "实际: {}", s);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        // After 2020 (the lifetime of this repository)
        assert!(s.as_str() > "2020-01-01T00:00:00Z", "实际: {}", s);
    }
}
