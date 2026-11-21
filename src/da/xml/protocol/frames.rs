use crate::da::xflash::protocol::{CMD_MAGIC, DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};

use super::*;

impl XmlPacketHeader {
    /// Fixed header length
    pub const SIZE: usize = 12;

    /// Flow packet (normal flow)
    pub fn flow(length: u32) -> Self {
        XmlPacketHeader {
            magic: CMD_MAGIC,
            data_type: DATA_TYPE_FLOW,
            length,
        }
    }

    /// Parse a header; returns `None` if magic mismatches or the length is invalid
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != CMD_MAGIC {
            return None;
        }
        let data_type = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let length = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        Some(XmlPacketHeader {
            magic,
            data_type,
            length,
        })
    }

    /// Serialize to 12 bytes LE
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.magic.to_le_bytes());
        buf[4..8].copy_from_slice(&self.data_type.to_le_bytes());
        buf[8..12].copy_from_slice(&self.length.to_le_bytes());
        buf
    }

    /// Whether this is a device async message packet
    pub fn is_message(&self) -> bool {
        self.data_type == DATA_TYPE_MESSAGE
    }
}

// =============================================================================
// Lightweight XML parsing
// =============================================================================

/// Get the text content at a tag path (e.g. `"arg/message"`)
pub fn get_tag(xml: &str, path: &str) -> Result<String, String> {
    let mut current = xml.to_string();
    for seg in path.split('/') {
        current = extract_tag(&current, seg).ok_or_else(|| format!("XML 标签 `{}` 未找到", seg))?;
    }
    Ok(current.trim().to_string())
}

/// Get a tag path and parse it as hex (accepts a `0x` prefix), e.g. `"arg/packet_length"`
pub fn get_tag_usize(xml: &str, path: &str) -> Result<usize, String> {
    let raw = get_tag(xml, path)?;
    let trimmed = raw.trim_start_matches("0x");
    usize::from_str_radix(trimmed, 16)
        .map_err(|_| format!("XML 标签 `{}` 不是合法十六进制: {}", path, raw))
}

/// Extract the inner text of the first `<tag>...</tag>`
fn extract_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    Some(content[start..end].to_string())
}
