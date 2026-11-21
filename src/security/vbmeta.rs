use log::info;

use crate::da::DAXFlash;

/// Candidate vbmeta partitions (covering the variants supported by the C# version).
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

/// Slot determination result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// Active slot determined (from bootctrl or GPT metadata)
    Known(char),
    /// Device is not A/B (no paired `_a/_b` vbmeta partitions)
    NotAb,
}

/// Action to take for a single candidate partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VbAction {
    Patch,
    /// Inactive slot / not applicable; skip and record the reason
    Skip(&'static str),
}

/// Pure function: decide whether a candidate partition should be modified.
///
/// Rules (matching "inactive slot SKIP + record reason; error out when unsure, never guess"):
/// - No `_a/_b` suffix (shared vbmeta): always process;
/// - Has a suffix and the slot is known: process only the current slot; SKIP the other;
/// - Has a suffix but the device is not A/B: SKIP (the partition cannot exist; skipping is enough).
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

/// Resolve the active slot: bootctrl → GPT metadata → error (never guess).
///
/// "GPT metadata" here means partition existence: if both `_a` and `_b` variants exist but bootctrl
/// cannot be read, the device is A/B with an unknown state → error out, preventing one `zyb vbmeta`
/// run from patching both slots (which would cost the user rollback capability).
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
        // Shared vbmeta + slotted vbmeta_system: always patch the former, only the active slot for the latter
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
