/// Incremental CRC32 (IEEE 802.3, matching the GPT checksum).
///
/// Uses a lookup table: a bitwise implementation would need about 500 million iterations for 64 MiB, too slow.
#[derive(Debug, Clone)]
pub struct Crc32 {
    state: u32,
}

const fn make_crc_table() -> [u32; 256] {
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

static CRC_TABLE: [u32; 256] = make_crc_table();

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

/// One-shot CRC32 calculation.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
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
