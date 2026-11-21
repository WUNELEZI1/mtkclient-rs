//! DA1 / DA2 patches module
//!
//! - `find_binary`         — byte search supporting `.` (0x2E) wildcards
//! - `apply_patches`       — applies a series of (pattern, replacement, name) patches
//! - `COMMON_PATCHES`      — patch table shared by DA1/DA2
//! - `DAXFlash::patch_da1` — DA1-specific patches
//! - `DAXFlash::patch_da2` — DA2-specific patches

use log::{debug, info, warn};

use crate::da::DAXFlash;

/// Search for a pattern in the binary data (supports `.` 0x2E as a single-byte wildcard)
/// Matches Python utils.py find_binary()
pub fn find_binary(data: &[u8], pattern: &[u8], start_pos: usize) -> Option<usize> {
    if pattern.is_empty() || start_pos >= data.len() {
        return None;
    }
    let search_data = &data[start_pos..];

    // Split into segments on 0x2E ('.') ('.' is the wildcard)
    let mut segments: Vec<&[u8]> = Vec::new();
    let mut last = 0;
    for (i, &b) in pattern.iter().enumerate() {
        if b == b'.' {
            if i > last {
                segments.push(&pattern[last..i]);
            }
            last = i + 1;
        }
    }
    if last < pattern.len() {
        segments.push(&pattern[last..]);
    }

    // If there is no wildcard, match exactly
    if segments.len() <= 1 {
        return search_data
            .windows(pattern.len())
            .position(|w| w == pattern)
            .map(|p| p + start_pos);
    }

    // Search the first segment, and at each position check the following segments (skipping wildcard bytes)
    let seg0 = segments[0];
    let mut search_start = 0;
    while search_start + seg0.len() <= search_data.len() {
        if let Some(idx) = search_data[search_start..]
            .windows(seg0.len())
            .position(|w| w == seg0)
        {
            let base = search_start + idx;
            let mut pos = base + seg0.len();
            let mut ok = true;
            for seg in &segments[1..] {
                pos += 1; // skip wildcard byte (.)
                if pos + seg.len() > search_data.len() {
                    ok = false;
                    break;
                }
                if search_data[pos..pos + seg.len()] != **seg {
                    ok = false;
                    break;
                }
                pos += seg.len();
            }
            if ok {
                return Some(base + start_pos);
            }
            search_start = base + 1;
        } else {
            break;
        }
    }
    None
}

/// Patches common to DA1/DA2 (the same 8 patches)
const COMMON_PATCHES: &[(&[u8], &[u8], &str)] = &[
    (
        b"\xA3\x68\x7B\xB1\x28\x46",
        b"\x01\x23\xA3\x60\x28\x46",
        "oppo security",
    ),
    (
        b"\xB3\xF5\x80\x7F\x01\xD1",
        b"\xB3\xF5\x80\x7F\x01\xD1\x4F\xF0\x00\x00\x4F\xF0\x00\x00\x70\x47",
        "mt6739 c30",
    ),
    (
        b"\xB3\xF5\x80\x7F\x04\xBF\x4F\xF4\x80\x73\x05\xF0\x11\xB8\x4F\xF0\xFF\x30\x70\x47",
        b"\xB3\xF5\x80\x7F\x04\xBF\x4F\xF4\x80\x73\x4F\xF0\x00\x00\x4F\xF0\x00\x00\x70\x47",
        "regular",
    ),
    (
        b"\x10\xB5\x0C\x68\x02\x68",
        b"\x10\xB5\x01\x20\x10\xBD",
        "ram blacklist",
    ),
    (
        b"\x08\xB5\x10\x4B\x7B\x44\x1B\x68\x1B\x68",
        b"\x00\x20\x70\x47\x00\x00\x00\x00\x00\x00",
        "seclib_sec_usbdl_enabled",
    ),
    (b"Preloader Start", b"Patched L Start", "Patched loader msg"),
    (
        b"\xF0\xB5\x8B\xB0\x02\xAE\x20\x25\x0C\x46\x07\x46",
        b"\x00\x20\x70\x47\x00\x00\x00\x00\x00\x20\x53\x74\x61\x72\x74",
        "sec_img_auth",
    ),
    (
        b"\xFF\xC0\xF3\x40\x00\x08\xBD",
        b"\xFF\x4F\xF0\x00\x00\x08\xBD",
        "get_vfy_policy",
    ),
];

/// Apply patches, returning the number of successfully applied patches
/// With `log_individual` = true each patch logs via info!, otherwise via debug!
fn apply_patches(
    data: &mut [u8],
    patches: &[(&[u8], &[u8], &str)],
    tag: &str,
    log_individual: bool,
) -> u32 {
    let mut count: u32 = 0;
    for (pattern, replacement, name) in patches {
        if let Some(idx) = find_binary(data, pattern, 0) {
            let end = idx + replacement.len();
            if end <= data.len() {
                data[idx..end].copy_from_slice(replacement);
                if log_individual {
                    info!("已修补 {}: {}", tag, name);
                } else {
                    debug!("已修补 {}: {}", tag, name);
                }
                count += 1;
            }
        }
    }
    count
}

/// DA1 patch: matches Python patch_da1 + patch_preloader_security_da1
pub fn patch_da1(data: &mut [u8]) {
    let mut patch_count = apply_patches(data, COMMON_PATCHES, "DA1", false);

    // DA1-specific: hash_check3
    let hash3_pattern = b"\x14\x2C\xF6\x2E\xFE\xE7";
    if let Some(idx) = find_binary(data, hash3_pattern, 0) {
        let replacement = b"\x00\x00\x00\x00\x00\x00";
        let end = idx + replacement.len();
        if end <= data.len() {
            data[idx..end].copy_from_slice(replacement);
            debug!("已修补 DA1: hash_check3");
            patch_count += 1;
        }
    }

    // DA1-specific: da_version_check
    if let Some(idx) = find_binary(data, b"\x1F\xB5\x00\x23\x01\xA8\x00\x93\x00\xF0", 0) {
        data[idx..idx + 4].copy_from_slice(b"\x00\x20\x70\x47");
        debug!("已修补 DA1: 版本检查");
        patch_count += 1;
    } else {
        warn!("DA1 version check 模式未找到");
    }

    // DA1-specific: hash_check
    if let Some(idx) = find_binary(data, b"\x04\x00\x07\xC0", 0) {
        let end = idx + 4;
        if end <= data.len() {
            data[idx..end].copy_from_slice(b"\x00\x00\x00\x00");
            debug!("已修补 DA1: hash_check");
            patch_count += 1;
        }
    }

    // DA1-specific: hash_check2
    if let Some(idx) = find_binary(data, b"\xCC\xF2\x07\x09", 0) {
        let replacement = b"\x4F\xF0\x00\x09";
        let end = idx + replacement.len();
        if end <= data.len() {
            data[idx..end].copy_from_slice(replacement);
            debug!("已修补 DA1: hash_check2");
            patch_count += 1;
        }
    }

    if patch_count == 0 {
        warn!("DA1 无修补应用");
    } else {
        info!("已修补 DA1: {} 个补丁已应用", patch_count);
    }
}

/// DA2 patch: matches Python patch_da2 + patch_preloader_security_da2
pub fn patch_da2(data: &mut [u8]) {
    let mut patch_count = apply_patches(data, COMMON_PATCHES, "DA2", true);

    // DA2-specific: huawei security
    if let Some(idx) = find_binary(data, b"\x01\x2B\x03\xD1\x01\x23", 0) {
        data[idx..idx + 4].copy_from_slice(b"\x00\x00\x00\x00");
        info!("已修补 DA2: huawei security");
        patch_count += 1;
    }

    // DA2-specific: oppo security (complex logic)
    if find_binary(data, b"[oplus]", 0).is_some() || find_binary(data, b"[OPPO]", 0).is_some() {
        if let Some(oppo) = find_binary(data, b"\x0A\x00\x00\xE0.\x00\x00\xE0", 0)
            && oppo >= 4
        {
            let auth_flag_ptr = u32::from_le_bytes(data[oppo - 4..oppo].try_into().unwrap());
            info!(
                "已修补 DA2: oppo security (mt6765, ptr=0x{:08X})",
                auth_flag_ptr
            );
            patch_count += 1;
        }

        let mut oppo_pos = 0;
        let mut oppo_patched = false;
        while oppo_pos < data.len() {
            if let Some(oppo) = data[oppo_pos..]
                .windows(6)
                .position(|w| w == b"\x01\x3B\x01\x2B\x08\xD9")
            {
                let actual = oppo_pos + oppo;
                data[actual..actual + 4].copy_from_slice(b"\x01\x20\x08\xBD");
                oppo_patched = true;
                oppo_pos = actual + 1;
            } else {
                break;
            }
        }
        if oppo_patched {
            info!("已修补 DA2: oppo security (loop)");
            patch_count += 1;
        }
    }

    // DA2-specific: hash binding
    if let Some(idx) = find_binary(data, b"\x01\x23\x03\x60\x00\x20\x70\x47\x70\xB5", 0) {
        data[idx..idx + 1].copy_from_slice(b"\x00");
        info!("已修补 DA2: hash binding");
        patch_count += 1;
    }

    // DA2-specific: hash check
    if let Some(idx) = find_binary(data, &0xC0070004u32.to_le_bytes(), 0) {
        data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
        info!("已修补 DA2: hash check (0xC0070004)");
        patch_count += 1;
    } else if let Some(idx) = find_binary(data, b"\x4F\xF0\x04\x09\xCC\xF2\x07\x09", 0) {
        data[idx..idx + 8].copy_from_slice(b"\x4F\xF0\x00\x09\x4F\xF0\x00\x09");
        info!("已修补 DA2: hash check (arm pattern)");
        patch_count += 1;
    } else if let Some(idx) = find_binary(
        data,
        b"\x4F\xF0\x04\x09\x32\x46\x01\x98\x03\x99\xCC\xF2\x07\x09",
        0,
    ) {
        data[idx..idx + 14]
            .copy_from_slice(b"\x4F\xF0\x00\x09\x32\x46\x01\x98\x03\x99\x4F\xF0\x00\x09");
        info!("已修补 DA2: hash check (arm pattern 2)");
        patch_count += 1;
    } else {
        warn!("DA2 hash check 未找到");
    }

    // DA2-specific: security check
    if let Some(idx) = find_binary(data, b"\x01\x23\x03\x60\x00\x20\x70\x47\x70\xB5", 0) {
        data[idx..idx + 2].copy_from_slice(b"\x00\x23");
        info!("已修补 DA2: security check");
        patch_count += 1;
    }

    // DA2-specific: anti-rollback
    if let Some(idx) = find_binary(data, &0xC0020053u32.to_le_bytes(), 0) {
        data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
        info!("已修补 DA2: DA version anti-rollback");
        patch_count += 1;
    }

    // DA2-specific: SBC
    if let Some(idx) = find_binary(data, b"\x02\x4B\x18\x68\xC0\xF3\x40\x00\x70\x47", 0) {
        data[idx + 4..idx + 8].copy_from_slice(b"\x4F\xF0\x00\x00");
        info!("已修补 DA2: SBC");
        patch_count += 1;
    }

    // DA2-specific: register read/write
    if let Some(idx) = find_binary(data, &0xC004000Du32.to_le_bytes(), 0) {
        data[idx..idx + 4].copy_from_slice(&0u32.to_le_bytes());
        info!("已修补 DA2: register read/write");
        patch_count += 1;
    }

    // DA2-specific: write not allowed pattern 1
    let mut idx = 0;
    let mut write_patched = false;
    while let Some(pos) = data[idx..]
        .windows(8)
        .position(|w| w == b"\x37\xB5\x00\x23\x04\x46\x02\xA8")
    {
        let actual_idx = idx + pos;
        data[actual_idx..actual_idx + 8].copy_from_slice(b"\x37\xB5\x00\x20\x03\xB0\x30\xBD");
        write_patched = true;
        idx = actual_idx + 1;
    }
    if write_patched {
        info!("已修补 DA2: write not allowed (pattern 1)");
        patch_count += 1;
    }

    // DA2-specific: write not allowed pattern 2
    if let Some(idx) = find_binary(data, b"\x0C\x23\xCC\xF2\x02\x03", 0) {
        data[idx..idx + 6].copy_from_slice(b"\x00\x23\x00\x23\x00\x23");
        if let Some(idx2) = find_binary(data, b"\x2A\x23\xCC\xF2\x02\x03", 0) {
            data[idx2..idx2 + 6].copy_from_slice(b"\x00\x23\x00\x23\x00\x23");
        }
        info!("已修补 DA2: write not allowed (pattern 2)");
        patch_count += 1;
    }

    if patch_count == 0 {
        warn!("DA2 无修补应用");
    } else {
        info!("已修补 DA2: 共 {} 个补丁", patch_count);
    }
}

// =============================================================================
// impl DAXFlash — lets upload.rs call via Self::patch_da1 / Self::patch_da2
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// DA1 patch: matches Python patch_da1 + patch_preloader_security_da1
    pub(crate) fn patch_da1(data: &mut [u8]) {
        patch_da1(data);
    }

    /// DA2 patch: matches Python patch_da2 + patch_preloader_security_da2
    pub(crate) fn patch_da2(data: &mut [u8]) {
        patch_da2(data);
    }
}
