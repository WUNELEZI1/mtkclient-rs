//! MTK AllInOne DA header parsing (pure data processing, no USB traffic)
//!
//! - `DaRegion`         — metadata for a single region in the DA file
//! - `parse_da_regions` — parse the region list from the DA header area
//! - `parse_da_header`  — parse an entire AllInOne DA file

use log::{info, warn};

/// DA file region structure
#[derive(Debug, Clone)]
pub struct DaRegion {
    pub(crate) buf_offset: u32, // offset within the file
    pub(crate) len: u32,        // size
    pub(crate) start_addr: u32, // load address
    pub(crate) sig_len: u32,    // signature length
}

/// Parse region information from the DA header data
pub(crate) fn parse_da_regions(
    header: &[u8],
    region_start: usize,
    entry_region_count: u16,
) -> Vec<DaRegion> {
    let mut regions = Vec::new();

    for j in 0..entry_region_count as usize {
        let region_offset = region_start + j * 20;
        if region_offset + 20 > header.len() {
            break;
        }

        let buf_offset = u32::from_le_bytes([
            header[region_offset],
            header[region_offset + 1],
            header[region_offset + 2],
            header[region_offset + 3],
        ]);
        let len = u32::from_le_bytes([
            header[region_offset + 4],
            header[region_offset + 5],
            header[region_offset + 6],
            header[region_offset + 7],
        ]);
        let start_addr = u32::from_le_bytes([
            header[region_offset + 8],
            header[region_offset + 9],
            header[region_offset + 10],
            header[region_offset + 11],
        ]);
        let sig_len = u32::from_le_bytes([
            header[region_offset + 16],
            header[region_offset + 17],
            header[region_offset + 18],
            header[region_offset + 19],
        ]);

        regions.push(DaRegion {
            buf_offset,
            len,
            start_addr,
            sig_len,
        });
    }

    regions
}

// Parse an MTK AllInOne DA file
pub(crate) fn parse_da_header(
    da_data: &[u8],
    target_hw_code: u16,
) -> Result<(u32, Vec<DaRegion>, bool), String> {
    if da_data.len() < 0x6C {
        return Err("DA 文件太小，无法解析头".to_string());
    }

    // Check whether this is the MTK AllInOne DA format
    let header = &da_data[0..0x68];
    let is_allinone = header.contains(&0x4D) && header.contains(&0x54) && header.contains(&0x4B);

    if is_allinone {
        // Read the DA count
        let count_da =
            u32::from_le_bytes([da_data[0x68], da_data[0x69], da_data[0x6A], da_data[0x6B]]);

        // Check whether this is the v6 format
        let is_v6 = header.windows(8).any(|window| window == b"MTK_DA_v6");

        // Check whether this is an old loader
        let old_ldr: bool;
        let offset: usize;

        if da_data.len() > 0x6C + 0xD8 + 1 {
            let marker = [da_data[0x6C + 0xD8], da_data[0x6C + 0xD8 + 1]];
            if marker == [0xDA, 0xDA] {
                offset = 0xD8;
                old_ldr = true;
            } else {
                offset = 0xDC;
                old_ldr = false;
            }
        } else {
            offset = 0xDC;
            old_ldr = false;
        }

        // Find the DA matching the target HW Code
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];

            // Read DA information
            let magic = u16::from_le_bytes([da_header[0], da_header[1]]);
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);
            let _hw_sub_code = u16::from_le_bytes([da_header[4], da_header[5]]);
            let _hw_version = u16::from_le_bytes([da_header[6], da_header[7]]);

            // Newer loaders carry sw_version + reserved1 (4 bytes)
            let _sw_version: u16;
            let _page_size: u16;
            let _entry_region_index: u16;
            let entry_region_count: u16;
            let region_start: usize;

            if old_ldr {
                _sw_version = 0;
                _page_size = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved = u16::from_le_bytes([da_header[10], da_header[11]]);
                _entry_region_index = u16::from_le_bytes([da_header[12], da_header[13]]);
                entry_region_count = u16::from_le_bytes([da_header[14], da_header[15]]);
                region_start = 16;
            } else {
                _sw_version = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved1 = u16::from_le_bytes([da_header[10], da_header[11]]);
                _page_size = u16::from_le_bytes([da_header[12], da_header[13]]);
                let _reserved3 = u16::from_le_bytes([da_header[14], da_header[15]]);
                _entry_region_index = u16::from_le_bytes([da_header[16], da_header[17]]);
                entry_region_count = u16::from_le_bytes([da_header[18], da_header[19]]);
                region_start = 20;
            }

            // Skip non-matching DAs silently
            if hw_code != target_hw_code {
                continue;
            }

            info!("找到匹配的 DA {}，HW Code: 0x{:04X}", i, hw_code);

            let regions = parse_da_regions(da_header, region_start, entry_region_count);

            return Ok((magic as u32, regions, is_v6));
        }

        // No DA matched the HW Code: fall back to the first valid DA (hw_code != 0)
        warn!(
            "未找到 HW Code 0x{:04X} 的 DA，尝试使用第一个有效的 DA",
            target_hw_code
        );
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);

            if hw_code != 0 {
                let region_start = da_offset + 16;
                let entry_region_count = if offset >= 16 {
                    u16::from_le_bytes([da_header[14], da_header[15]])
                } else {
                    0
                };

                let mut regions = parse_da_regions(da_header, region_start, entry_region_count);

                // No regions parsed: use the default Stage1 and Stage2 regions
                if regions.is_empty() {
                    warn!("备选 DA 没有 region 信息，使用默认值");
                    // Default Stage1 region
                    regions.push(DaRegion {
                        buf_offset: 0x376C,
                        len: 0x270,
                        start_addr: 0x200000,
                        sig_len: 0x0,
                    });
                    // Default Stage2 region
                    regions.push(DaRegion {
                        buf_offset: 0x39E4,
                        len: 0xE660,
                        start_addr: 0x80000000,
                        sig_len: 0x100,
                    });
                }

                info!("使用 DA {} (HW Code: 0x{:04X}) 作为备选", i, hw_code);
                return Ok((0, regions, is_v6));
            }
        }

        Err("在 AllInOne DA 中未找到有效的 DA 配置".to_string())
    } else {
        // Legacy DA format parsing
        let magic = u32::from_le_bytes([da_data[0], da_data[1], da_data[2], da_data[3]]);
        let hw_code = u16::from_le_bytes([da_data[4], da_data[5]]);
        let entry_region_count = u16::from_le_bytes([da_data[16], da_data[17]]);

        if hw_code != target_hw_code {
            warn!(
                "DA HW Code (0x{:04X}) 与目标 (0x{:04X}) 不匹配",
                hw_code, target_hw_code
            );
        }

        let is_v6 = magic == 0x5644415F;

        let regions = parse_da_regions(da_data, 0x6C, entry_region_count);

        Ok((magic, regions, is_v6))
    }
}

/// Carbonara vulnerability patch detection
/// Mirrors mtkclient Tools/da_parser.py
///
/// Returns true when the DA is patched (not vulnerable to Carbonara)
pub fn detect_carbonara_patch(da1_data: &[u8]) -> (bool, &'static str) {
    // Reuse the 5 patterns from the exploit registry so the two implementations do not drift
    // (this function used to check only 3 and missed "2nd DA address is invalid" and the V6 penumbra variant).
    for (i, p) in crate::exploit::registry::CARBONARA_PATTERNS
        .iter()
        .enumerate()
    {
        if da1_data.windows(p.len()).any(|w| w == *p) {
            return (true, CARBONARA_PATTERN_LABELS[i]);
        }
    }
    if da1_forces_da2_addr(da1_data) {
        return (true, "DA1 硬编码 DA2 加载地址 0x40000000（疑似加固）");
    }
    (false, "Carbonara vulnerable")
}

/// Readable labels for the 5 patterns (same order as `CARBONARA_PATTERNS`).
pub const CARBONARA_PATTERN_LABELS: [&str; 5] = [
    "V6 Carbonara patched (pattern 1)",
    "V6 Carbonara patched (pattern 2)",
    "V6 Carbonara patched (pattern 3, penumbra)",
    "V6 Carbonara patched (2nd DA address is invalid)",
    "V5 Carbonara patched",
];

/// A Carbonara-hardened DA1 hardcodes the DA2 load address to `0x40000000`, ignoring the host value.
pub const CARBONARA_FORCED_ADDR: u32 = 0x4000_0000;

/// Whether DA1 shows signs of forcing the DA2 load address to 0x40000000.
///
/// Two shapes:
/// - a literal in the constant pool (little-endian `00 00 00 40` / big-endian `40 00 00 00`);
/// - an ARM `MOVT rX, #0x4000` encoding (little-endian `00 00 40 E3`).
///
/// This is a **heuristic** only: 0x40000000 may just be an ordinary memory base constant, so
/// callers should treat it as "suspicious" (`Confidence::Low`), not as proof of a patch.
pub fn da1_forces_da2_addr(da1: &[u8]) -> bool {
    let le = CARBONARA_FORCED_ADDR.to_le_bytes();
    let be = CARBONARA_FORCED_ADDR.to_be_bytes();
    da1.windows(4).any(|w| w == le || w == be)
        || da1.windows(4).any(|w| w[2] == 0x40 && w[3] == 0xE3)
}

/// Hash check detection (whether the DA contains hash verification logic)
pub fn detect_hash_check(da1_data: &[u8]) -> (bool, &'static str) {
    // DA_HASH_MISMATCH error code 0xC0070004
    let hash_bytes = 0xC0070004u32.to_le_bytes();
    if da1_data.windows(4).any(|w| w == hash_bytes) {
        return (true, "Hash check v1 found");
    }
    // Alternative patterns from mtkclient
    if da1_data.windows(4).any(|w| w == b"\xCC\xF2\x07\x09") {
        return (true, "Hash check v2 found");
    }
    if da1_data
        .windows(6)
        .any(|w| &w[..2] == b"\x14\x2C" && w[4..6] == [0xFE, 0xE7])
    {
        return (true, "Hash check v3 found");
    }
    if da1_data
        .windows(8)
        .any(|w| w == b"\x04\x50\x00\xE3\x07\x50\x4C\xE3")
    {
        return (true, "Hash check v4 (V6) found");
    }
    if da1_data
        .windows(8)
        .any(|w| w == b"\x01\x10\x81\xE2\x00\x00\x51\xE1")
    {
        return (true, "Hash check v5 (V6) found");
    }
    (false, "No hash check detected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exploit::registry::CARBONARA_PATTERNS;

    #[test]
    fn detect_carbonara_covers_all_five_patterns() {
        for (i, p) in CARBONARA_PATTERNS.iter().enumerate() {
            let mut da1 = vec![0u8; 32];
            da1.extend_from_slice(p);
            let (patched, label) = detect_carbonara_patch(&da1);
            assert!(patched, "pattern {} 未命中: {:?}", i, label);
            assert_eq!(label, CARBONARA_PATTERN_LABELS[i]);
        }
    }

    #[test]
    fn detect_forced_da2_address_as_heuristic() {
        let mut da1 = vec![0u8; 16];
        da1.extend_from_slice(&CARBONARA_FORCED_ADDR.to_le_bytes());
        assert!(da1_forces_da2_addr(&da1));
        assert!(detect_carbonara_patch(&da1).0);
    }

    #[test]
    fn clean_da1_reports_vulnerable() {
        let da1 = vec![0x11u8; 128];
        let (patched, label) = detect_carbonara_patch(&da1);
        assert!(!patched);
        assert_eq!(label, "Carbonara vulnerable");
        assert!(!da1_forces_da2_addr(&da1));
    }
}
