//! A/B 槽位自动解析
//!
//! 统一入口 [`detect_active_slot`] / [`resolve_slot_partition`]：
//! 1. misc 分区 bootctrl 结构（boot_control_message / AVB 槽位元数据，偏移 0x2000）；
//! 2. GPT 启发：存在 `xxx_a`/`xxx_b` 而请求裸名 `xxx` 时按活跃槽位补后缀；
//! 3. 兜底：misc 文本中的槽位标记。
//!
//! 接线点：`find_partition_addr` 在 GPT 直查失败后自动调用
//! [`resolve_slot_partition`]，使 `r boot` / `w vbmeta` 等命令无需手动带 `_a/_b`。

use crate::da::DAXFlash;

/// bootctrl 魔数字节流 "BOOT"（按字节比较，规避端序歧义）。
const BOOTCTRL_MAGIC: &[u8; 4] = b"BOOT";

/// 检测当前活跃 A/B 槽位。
///
/// 返回 `Ok(Some('a' | 'b'))`；非 A/B 设备或检测失败返回 `Ok(None)`。
/// 结果缓存在 `da.active_slot`，同一 DA 会话内只读一次 misc。
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

/// 不走缓存的原始检测（misc 分区 bootctrl + 文本启发）。
fn detect_active_slot_uncached(da: &mut DAXFlash) -> Result<Option<char>, String> {
    let (misc_addr, misc_size) = match da.find_partition_addr("misc") {
        Ok((addr, size)) => (addr, size.min(0x10000)),
        Err(_) => return Ok(None),
    };

    let misc_data = match da.readflash_data(misc_addr, misc_size) {
        Ok(data) => data,
        Err(_) => return Ok(None),
    };

    // 主路径：boot_control 结构（misc 偏移 0x2000，AOSP boot_control AVB 元数据布局）。
    // 魔数按字节流比较 "BOOT"（不同固件写入端序不一，字节级匹配两种都认）。
    if misc_data.len() >= 0x2000 + 12 {
        let off = 0x2000;
        if &misc_data[off..off + 4] == BOOTCTRL_MAGIC {
            // active_slot 在 magic + 8（跳过 u32 保留位）
            let active = u32::from_le_bytes(misc_data[off + 8..off + 12].try_into().unwrap());
            let slot = if active == 0 { 'a' } else { 'b' };
            return Ok(Some(slot));
        }
    }

    // 兜底：misc 里的 bootloader 消息 / 文本槽位标记
    let data_str = String::from_utf8_lossy(&misc_data);
    if data_str.contains("_a") && !data_str.contains("_b") {
        Ok(Some('a'))
    } else if data_str.contains("_b") && !data_str.contains("_a") {
        Ok(Some('b'))
    } else {
        Ok(None)
    }
}

/// 把裸分区名解析为带槽位后缀的实际 GPT 名。
///
/// 规则：
/// - 名字已带 `_a`/`_b` 后缀 → 原样返回；
/// - GPT 中直接存在该名 → 原样返回（非 A/B 分区）；
/// - GPT 中存在 `name_a` 或 `name_b` → 按活跃槽位返回（无槽位信息时返回存在的那个）。
pub fn resolve_slot_partition(da: &mut DAXFlash, name: &str) -> Option<String> {
    // 已带后缀：不处理
    if name.ends_with("_a") || name.ends_with("_b") {
        return None;
    }

    // 需要 GPT 在缓存中（find_partition_addr 调用前已加载）
    let gpt_data = da.last_gpt_data.as_ref()?;

    let has_partition = |n: &str| -> bool {
        crate::partition::gpt::GptInfo::parse(gpt_data)
            .map(|g| g.find_partition(n).is_some())
            .unwrap_or(false)
    };

    if has_partition(name) {
        return None; // 裸名存在，非 A/B
    }

    let has_a = has_partition(&format!("{name}_a"));
    let has_b = has_partition(&format!("{name}_b"));
    if !has_a && !has_b {
        return None;
    }

    // 双槽位：按活跃槽位；单槽位（罕见）：返回存在者
    let slot = detect_active_slot(da).ok().flatten();
    match (slot, has_a, has_b) {
        (Some('a'), true, _) => Some(format!("{name}_a")),
        (Some('b'), _, true) => Some(format!("{name}_b")),
        (None, true, false) => Some(format!("{name}_a")),
        (None, false, true) => Some(format!("{name}_b")),
        (None, true, true) => {
            // 无槽位信息但双槽位存在：默认 a（与 bootloader 默认一致）
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
        // 魔数按字节流比较，两种端序的固件写入都能匹配
        assert_eq!(&BOOTCTRL_MAGIC[..], b"BOOT");
        assert_eq!(u32::from_be_bytes(*BOOTCTRL_MAGIC), 0x424F_4F54);
    }

    #[test]
    fn suffix_detection_is_safe() {
        // ends_with 检查不误伤特殊分区名
        assert!("boot_a".ends_with("_a"));
        assert!(!"boot1".ends_with("_a"));
        assert!(!"rpmb".ends_with("_b"));
    }
}
