//! MTK proinfo partition parser (pure parser).
//!
//! Note: MTK proinfo has no globally unified format; it varies widely by model and generation.
//! Two strategies are implemented:
//!    1) If the partition starts with the ASCII magic "PROINFO", parse as `version(u32) + count(u32)`
//!       followed by `{name[32], offset(u32), length(u32)}` block-table entries;
//!    2) Otherwise scan heuristically for candidate block-table entries of
//!       "32 printable bytes name + plausible offset/length".
//!
//! If the magic/layout differs on a real device, calibrate per model.
//! This module performs no device I/O and prints nothing; the command layer owns
//! all user-facing output.

#[derive(Debug, Clone)]
pub struct ProInfoBlock {
    pub name: String,
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug)]
pub struct ProInfo {
    pub magic: Option<String>,
    pub version: Option<u32>,
    pub blocks: Vec<ProInfoBlock>,
    pub scan: Vec<ProInfoBlock>,
}

pub fn parse(buf: &[u8]) -> ProInfo {
    let mut info = ProInfo {
        magic: None,
        version: None,
        blocks: Vec::new(),
        scan: Vec::new(),
    };

    if buf.len() >= 16 && &buf[0..7] == b"PROINFO" {
        info.magic = Some("PROINFO".into());
        info.version = Some(u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]));
        let count = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
        let mut p = 16;
        for _ in 0..count.min(128) {
            if p + 40 > buf.len() {
                break;
            }
            let name = String::from_utf8_lossy(&buf[p..p + 32])
                .trim_end_matches('\0')
                .to_string();
            let offset = u32::from_le_bytes([buf[p + 32], buf[p + 33], buf[p + 34], buf[p + 35]]);
            let length = u32::from_le_bytes([buf[p + 36], buf[p + 37], buf[p + 38], buf[p + 39]]);
            if offset as usize <= buf.len() && length < 0x1_0000 && length > 0 {
                info.blocks.push(ProInfoBlock {
                    name,
                    offset,
                    length,
                });
            }
            p += 40;
        }
    } else {
        // Heuristic scan: step by 4 bytes looking for a 32-byte printable name + plausible offset/length
        let mut p = 0;
        while p + 40 <= buf.len() {
            let nb = &buf[p..p + 32];
            if nb
                .iter()
                .all(|&b| b == 0 || b.is_ascii_graphic() || b == b' ')
                && !nb.iter().all(|&b| b == 0)
            {
                let offset =
                    u32::from_le_bytes([buf[p + 32], buf[p + 33], buf[p + 34], buf[p + 35]]);
                let length =
                    u32::from_le_bytes([buf[p + 36], buf[p + 37], buf[p + 38], buf[p + 39]]);
                if offset % 4 == 0
                    && offset as usize <= buf.len()
                    && length < 0x4000
                    && length > 0
                    && offset != 0
                {
                    info.scan.push(ProInfoBlock {
                        name: String::from_utf8_lossy(nb)
                            .trim_end_matches('\0')
                            .to_string(),
                        offset,
                        length,
                    });
                }
            }
            p += 4;
        }
    }
    info
}
