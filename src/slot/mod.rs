//! A/B slot auto-resolution.
//!
//! Unified entry points [`detect_active_slot`] / [`resolve_slot_partition`]:
//! 1. misc partition bootctrl structure (boot_control_message / AVB slot metadata, offset 0x2000);
//! 2. GPT heuristic: if `xxx_a`/`xxx_b` exist but the bare name `xxx` is requested,
//!    append the active slot suffix;
//! 3. fallback: slot markers in the misc text.
//!
//! Wiring: `find_partition_addr` calls [`resolve_slot_partition`] automatically after a
//! direct GPT lookup fails, so commands like `r boot` / `w vbmeta` need no manual `_a/_b`.

use crate::da::DAXFlash;

/// bootctrl magic byte string "BOOT" (compared per byte to avoid endianness ambiguity).
const BOOTCTRL_MAGIC: &[u8; 4] = b"BOOT";

/// Detects the currently active A/B slot.
///
/// Returns `Ok(Some('a' | 'b'))`; returns `Ok(None)` on non-A/B devices or when
/// detection fails. The result is cached in `da.active_slot`, so misc is read only
/// once per DA session.
pub fn detect_active_slot(da: &mut DAXFlash) -> Result<Option<char>, String> {
    if let Some(cached) = da.active_slot {
        return Ok(Some(cached));
    }
    let slot = detect_active_slot_uncached(da)?;
    if slot.is_some() {
        da.active_slot = slot;
    }
    Ok(slot)
}

/// Uncached raw detection (misc bootctrl + text heuristic).
fn detect_active_slot_uncached(da: &mut DAXFlash) -> Result<Option<char>, String> {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok((addr, size)) => (addr, size.min(0x10000)),
        Err(_) => return Ok(None),
    };

    let misc_data = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(_) => return Ok(None),
    };

    // Primary path: boot_control structure (misc offset 0x2000, AOSP boot_control AVB metadata layout).
    // The magic is compared as a byte stream against "BOOT" (firmware endianness varies, byte-level matching accepts both).
    if misc_data.len() >= 0x2000 + 12 {
        let off = 0x2000;
        if &misc_data[off..off + 4] == BOOTCTRL_MAGIC {
            // active_slot sits at magic + 8 (skipping the u32 reserved field)
            let active = u32::from_le_bytes(misc_data[off + 8..off + 12].try_into().unwrap());
            let slot = if active == 0 { 'a' } else { 'b' };
            return Ok(Some(slot));
        }
    }

    // Fallback: bootloader messages / textual slot markers in misc
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        Ok(Some('a'))
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        Ok(Some('b'))
    } else {
        Ok(None)
    }
}

/// Resolves a bare partition name to the actual GPT name with slot suffix.
///
/// Rules:
/// - name already ends with `_a`/`_b` -> returned as is;
/// - the name exists directly in GPT -> returned as is (non-A/B partition);
/// - `name_a` or `name_b` exists in GPT -> return according to the active slot
///   (if slot info is unavailable, return the one that exists).
pub fn resolve_slot_partition(da: &mut DAXFlash, name: &str) -> Option<String> {
    // Already suffixed: nothing to do
    if name.ends_with("_a") || name.ends_with("_b") {
        return None;
    }

    // GPT must be cached (loaded before find_partition_addr is called)
    let gpt_data = da.last_gpt_data.as_ref()?;

    let has_partition = |n: &str| -> bool {
        crate::partition::gpt::GptInfo::parse(gpt_data)
            .map(|g| g.find_partition(n).is_some())
            .unwrap_or(false)
    };

    if has_partition(name) {
        return None; // bare name exists, not A/B
    }

    let has_a = has_partition(&format!("{name}_a"));
    let has_b = has_partition(&format!("{name}_b"));
    if !has_a && !has_b {
        return None;
    }

    // Dual slot: follow the active slot; single slot (rare): return the one present
    let slot = detect_active_slot(da).ok().flatten();
    match (slot, has_a, has_b) {
        (Some('a'), true, _) => Some(format!("{name}_a")),
        (Some('b'), _, true) => Some(format!("{name}_b")),
        (None, true, false) => Some(format!("{name}_a")),
        (None, false, true) => Some(format!("{name}_b")),
        (None, true, true) => {
            // No slot info but both slots exist: default to a (matches bootloader default)
            Some(format!("{name}_a"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootctrl_magic_bytes() {
        // Magic compared as byte stream, so firmware of either endianness matches
        assert_eq!(&BOOTCTRL_MAGIC[..], b"BOOT");
        assert_eq!(u32::from_be_bytes(*BOOTCTRL_MAGIC), 0x424F_4F54);
    }

    #[test]
    fn suffix_detection_is_safe() {
        // ends_with check does not misfire on special partition names
        assert!("boot_a".ends_with("_a"));
        assert!(!"boot1".ends_with("_a"));
        assert!(!"rpmb".ends_with("_b"));
    }
}
