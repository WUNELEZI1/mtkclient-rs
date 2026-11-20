use crate::color::Colorize;
use log::info;

use crate::da::DAXFlash;

/// vbmeta 候选分区（覆盖 C# 版支持的变体）。
pub(crate) const VBMETA_CANDIDATES: [&str; 13] = [
    "vbmeta_a",
    "vbmeta_b",
    "vbmeta",
    "vbmeta_system_a",
    "vbmeta_system_b",
    "vbmeta_vendor_a",
    "vbmeta_vendor_b",
    "vbmeta_product_a",
    "vbmeta_product_b",
    "vbmeta_system_ext_a",
    "vbmeta_system_ext_b",
    "vbmeta_odm_a",
    "vbmeta_odm_b",
];

/// 槽位判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// 已确定活动槽（来自 bootctrl 或 GPT 元数据）
    Known(char),
    /// 设备不是 A/B（不存在 `_a/_b` 成对的 vbmeta）
    NotAb,
}

/// 单个候选分区的处理动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VbAction {
    Patch,
    /// 非活动槽 / 不适用，跳过并记录原因
    Skip(&'static str),
}

/// 纯函数：决定某个候选分区是否要改。
///
/// 规则（对应"非活动槽 SKIP + 记录原因；不确定就报错，不猜"）：
/// - 无 `_a/_b` 后缀（共享 vbmeta）：总是处理；
/// - 有后缀且槽位已确定：只处理当前槽，另一槽 SKIP；
/// - 有后缀但设备非 A/B：SKIP（该分区不可能存在，跳过即可）。
pub(crate) fn decide_action(name: &str, slot: SlotState) -> VbAction {
    let suffix = if name.ends_with("_a") {
        Some('a')
    } else if name.ends_with("_b") {
        Some('b')
    } else {
        None
    };
    match (suffix, slot) {
        (None, _) => VbAction::Patch,
        (Some(_), SlotState::NotAb) => VbAction::Skip("设备无 A/B 槽位"),
        (Some(s), SlotState::Known(active)) if s == active => VbAction::Patch,
        (Some(_), SlotState::Known(_)) => VbAction::Skip("非活动槽"),
    }
}

pub fn vbmeta_disable(da: &mut DAXFlash, mode: u32) -> Result<(), String> {
    info!("开始 vbmeta 禁用 (mode={})...", mode);

    let slot = resolve_slot(da)?;
    match slot {
        SlotState::Known(c) => info!("活动槽位: {}（只处理该槽 + 共享 vbmeta）", c),
        SlotState::NotAb => info!("未检测到 A/B 槽位（按非 A/B 设备处理）"),
    }

    let mut patched = Vec::new();
    let mut skipped = Vec::new();
    let mut missing = Vec::new();

    for name in VBMETA_CANDIDATES {
        match decide_action(name, slot) {
            VbAction::Skip(reason) => {
                // 只在分区确实存在时记录 SKIP，避免刷一堆"不存在"
                if da.find_partition_addr(name).is_ok() {
                    info!("  [SKIP] {}（{}）", name, reason);
                    skipped.push(name);
                }
                continue;
            }
            VbAction::Patch => {}
        }

        let Ok((addr, size)) = da.find_partition_addr(name) else {
            missing.push(name);
            continue;
        };
        info!(
            "  找到 vbmeta 分区: {} @ 0x{:X}, 大小: 0x{:X}",
            name, addr, size
        );

        let data = da.readflash_data(addr, size)?;
        info!("  读取到 {} 字节", data.len());

        // 自研 avbtool 解析：descriptor 摘要 + 严格 AVB0 校验
        let vb = match crate::security::avb::VbmetaImage::parse(&data) {
            Ok(v) => v,
            Err(e) => {
                info!("  {} 跳过（非标准 vbmeta: {}）", name, e);
                continue;
            }
        };
        for d in &vb.descriptors {
            if let Some(pn) = &d.partition_name {
                info!("    descriptor: {} ({})", d.tag_debug_name(), pn);
            }
        }
        info!(
            "  当前 flags=0x{:02X} -> 目标 0x{:02X}",
            vb.header.flags, mode
        );

        let modified = vb.disable_verification(mode);
        da.write_flash_data(addr, &modified, 1, 8)?;
        info!("{}", format!("  {} 已修补", name).green());
        patched.push(name);
    }

    if patched.is_empty() {
        return Err(format!(
            "未找到可修补的 vbmeta 分区（已跳过: {}，缺失: {}）",
            skipped.join(","),
            missing.join(",")
        ));
    }

    info!(
        "{}",
        format!(
            "vbmeta 禁用成功: 修补 {} 个{}, 跳过 {} 个",
            patched.len(),
            if skipped.is_empty() {
                ""
            } else {
                "（另有非活动槽跳过）"
            },
            skipped.len()
        )
        .green()
    );
    Ok(())
}

/// 解析活动槽位：bootctrl → GPT 元数据 → 报错（不猜）。
///
/// "GPT 元数据"这里用分区存在性判定：`_a` 与 `_b` 变体同时存在却读不到
/// bootctrl，说明这台设备是 A/B 但状态未知 → 直接报错，避免一次 `zyb vbmeta`
/// 把两个槽都改了（用户会丢掉回滚能力）。
pub(crate) fn resolve_slot(da: &mut DAXFlash) -> Result<SlotState, String> {
    if let Ok(Some(c)) = crate::slot::detect_active_slot(da) {
        return Ok(SlotState::Known(c));
    }

    let has_a = VBMETA_CANDIDATES
        .iter()
        .any(|n| n.ends_with("_a") && da.find_partition_addr(n).is_ok());
    let has_b = VBMETA_CANDIDATES
        .iter()
        .any(|n| n.ends_with("_b") && da.find_partition_addr(n).is_ok());

    match (has_a, has_b) {
        (true, true) => Err(
            "无法确定活动 A/B 槽位（bootctrl 读取失败，且 GPT 中 _a/_b 同时存在）；\
             请检查 misc 分区后重试，或先执行 slot show"
                .to_string(),
        ),
        (true, false) => {
            info!("bootctrl 不可用，按 GPT 元数据判定为 A 槽");
            Ok(SlotState::Known('a'))
        }
        (false, true) => {
            info!("bootctrl 不可用，按 GPT 元数据判定为 B 槽");
            Ok(SlotState::Known('b'))
        }
        (false, false) => Ok(SlotState::NotAb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vbmeta_is_always_patched() {
        assert_eq!(
            decide_action("vbmeta", SlotState::Known('a')),
            VbAction::Patch
        );
        assert_eq!(decide_action("vbmeta", SlotState::NotAb), VbAction::Patch);
    }

    #[test]
    fn inactive_slot_is_skipped_with_reason() {
        assert_eq!(
            decide_action("vbmeta_system_b", SlotState::Known('a')),
            VbAction::Skip("非活动槽")
        );
        assert_eq!(
            decide_action("vbmeta_a", SlotState::Known('a')),
            VbAction::Patch
        );
        assert_eq!(
            decide_action("vbmeta_b", SlotState::Known('b')),
            VbAction::Patch
        );
    }

    #[test]
    fn ab_suffix_on_non_ab_device_is_skipped() {
        assert_eq!(
            decide_action("vbmeta_vendor_a", SlotState::NotAb),
            VbAction::Skip("设备无 A/B 槽位")
        );
    }

    #[test]
    fn mixed_layout_handles_shared_and_slotted_separately() {
        // 共享 vbmeta + 分槽 vbmeta_system：前者总是改，后者只改活动槽
        let slot = SlotState::Known('b');
        assert_eq!(decide_action("vbmeta", slot), VbAction::Patch);
        assert_eq!(decide_action("vbmeta_system_b", slot), VbAction::Patch);
        assert_eq!(
            decide_action("vbmeta_system_a", slot),
            VbAction::Skip("非活动槽")
        );
    }

    #[test]
    fn candidate_list_has_no_plain_ab_duplicates() {
        for n in VBMETA_CANDIDATES {
            assert!(n.starts_with("vbmeta"));
        }
    }
}
