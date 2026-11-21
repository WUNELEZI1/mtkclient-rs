// Preserve the historical path; the shared table builder now lives in `crate::util`.
use crate::util::CRC_TABLE;
pub use crate::util::crc32;
#[allow(unused_imports)]
pub(crate) use crate::util::make_crc_table;

/// Incremental CRC32 (IEEE 802.3, matching the GPT checksum).
///
/// Uses a lookup table: a bitwise implementation would need about 500 million iterations for 64 MiB, too slow.
#[derive(Debug, Clone)]
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Continue accumulating from a completed CRC value (to keep hashing the same chunk on resume).
    pub fn from_crc(crc: u32) -> Self {
        Self { state: !crc }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &b in data {
            let idx = ((crc ^ b as u32) & 0xFF) as usize;
            crc = (crc >> 8) ^ CRC_TABLE[idx];
        }
        self.state = crc;
    }

    pub fn finalize(&self) -> u32 {
        !self.state
    }
}

/// Source file "fingerprint": size + mtime in seconds.
pub fn file_signature(file: &str) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(file).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}
