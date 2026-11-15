//! 写入后的 AVB 验证状态体检（Phase 1.3）。
//!
//! 刷入 `boot` / `system` 这类参与 AVB 校验的分区后，如果设备的验证没有关闭，
//! 开机会校验失败——轻则进 fastboot，重则反复重启（"刷完变砖"）。
//! 本模块在写入成功后**强制**检查一次验证状态，开启则打印红字警告并给出补救命令。
//!
//! 判定优先级（拿得到就用，越靠前越权威）：
//! 1. 同槽位/全局 `vbmeta` 的 flags（`HASHTREE_DISABLED` / `VERIFICATION_DISABLED`）；
//! 2. `seccfg` 的 `lock_state` 与 `dm_verity_state`；
//! 3. 都拿不到 → `Unknown`，只记警告，不阻断（绝不能因为"查不到"就让刷机失败）。

use log::{info, warn};

use crate::color::Colorize;
use crate::security::avb::{FLAG_HASHTREE_DISABLED, FLAG_VERIFICATION_DISABLED, VbmetaImage};
use crate::security::seccfg::v4::SecCfgV4;
use crate::session::Session;

/// 验证状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationState {
    /// 验证已关闭（安全）。
    Disabled { source: String },
    /// 验证仍开启（刷修改过的镜像有变砖风险）。
    Enabled { source: String, detail: String },
    /// 无法确定。
    Unknown { reason: String },
}

/// 参与 AVB 校验、被改动后可能影响开机的分区（去掉槽位后缀后比较）。
const AVB_PARTITIONS: &[&str] = &[
    "boot",
    "init_boot",
    "vendor_boot",
    "recovery",
    "system",
    "system_ext",
    "vendor",
    "product",
    "odm",
    "dtbo",
];

/// 去掉 `_a` / `_b` 槽位后缀。
fn base_name(part: &str) -> &str {
    match part.rsplit_once('_') {
        Some((base, "a" | "b")) => base,
        _ => part,
    }
}

/// 该分区写入后是否需要做 AVB 体检。
///
/// `vbmeta*` 本身排除：写它往往正是"关验证"动作，不该反过来警告。
pub fn is_avb_protected(part: &str) -> bool {
    let base = base_name(part);
    if base.starts_with("vbmeta") {
        return false;
    }
    AVB_PARTITIONS.contains(&base)
}

/// 候选 vbmeta 分区（按优先级）。
fn vbmeta_candidates(part: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some((_, slot @ ("a" | "b"))) = part.rsplit_once('_') {
        out.push(format!("vbmeta_{}", slot));
        out.push(format!("vbmeta_system_{}", slot));
    }
    out.push("vbmeta".to_string());
    out.push("vbmeta_system".to_string());
    out
}

/// 检查设备当前的 AVB 验证状态。
pub fn check(session: &mut dyn Session, part: &str) -> VerificationState {
    // 1) vbmeta flags
    for name in vbmeta_candidates(part) {
        // 分区不存在就换下一个候选（`find_partition` 对缺失分区返回 Err）。
        let span = match session.find_partition(&name) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let len = span.size.min(0x1_0000);
        let data = match session.read_flash(span.addr, len) {
            Ok(d) => d,
            Err(e) => {
                return VerificationState::Unknown {
                    reason: format!("读取 {} 失败: {}", name, e),
                };
            }
        };
        if let Ok(img) = VbmetaImage::parse(&data) {
            let flags = img.header.flags;
            let disabled = flags & (FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED) != 0;
            let source = format!("{} flags=0x{:X}", name, flags);
            return if disabled {
                VerificationState::Disabled { source }
            } else {
                VerificationState::Enabled {
                    source,
                    detail: "vbmeta 未置位 HASHTREE_DISABLED / VERIFICATION_DISABLED".into(),
                }
            };
        }
        // 解析失败不算结论，继续看下一个候选 / 退到 seccfg。
    }

    // 2) seccfg 兜底
    if let Ok(span) = session.find_partition("seccfg")
        && let Ok(data) = session.read_flash(span.addr, span.size.min(0x4000))
        && let Ok(v4) = SecCfgV4::parse(&data)
    {
        let source = format!(
            "seccfg lock_state={} dm_verity_state={}",
            v4.lock_state, v4.dm_verity_state
        );
        return if v4.lock_state == 3 && v4.dm_verity_state == 0 {
            VerificationState::Disabled { source }
        } else {
            VerificationState::Enabled {
                source,
                detail: format!(
                    "设备{}锁且 dm_verity_state={}",
                    if v4.lock_state == 3 {
                        "已解"
                    } else {
                        "未解"
                    },
                    v4.dm_verity_state
                ),
            }
        };
    }

    VerificationState::Unknown {
        reason: "既读不到 vbmeta，也读不到可解析的 seccfg".into(),
    }
}

/// 写入成功后调用：验证未关闭则打红字警告（不阻断写入）。
pub fn warn_after_write(session: &mut dyn Session, part: &str) {
    if !is_avb_protected(part) {
        return;
    }
    match check(session, part) {
        VerificationState::Disabled { source } => {
            info!("AVB 体检通过: 验证已关闭（{}）", source);
        }
        VerificationState::Enabled { source, detail } => {
            eprintln!(
                "{}",
                format!(
                    "警告: 已写入 {}，但设备 AVB 验证仍开启（{}；{}）",
                    part, source, detail
                )
                .red()
                .bold()
            );
            eprintln!(
                "{}",
                format!(
                    "      未关闭验证时刷入修改过的镜像，开机可能校验失败、无法启动。\n\
                     \x20     建议先执行 `mtkclient-rs vbmeta disable-verification`（或解锁 BL）再刷。"
                )
                .red()
            );
        }
        VerificationState::Unknown { reason } => {
            warn!(
                "写入 {} 后无法确认 AVB 验证状态（{}）；如开机失败请检查 vbmeta",
                part, reason
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avb_partition_classification() {
        for p in [
            "boot",
            "boot_a",
            "boot_b",
            "init_boot_a",
            "system",
            "vendor_b",
            "dtbo",
        ] {
            assert!(is_avb_protected(p), "{} 应受 AVB 保护", p);
        }
        for p in ["vbmeta", "vbmeta_a", "userdata", "persist", "nvram"] {
            assert!(!is_avb_protected(p), "{} 不应触发 AVB 警告", p);
        }
    }

    fn ready_session(profile: crate::transport::vdev::DeviceProfile) -> crate::session::SimSession {
        let dev = crate::transport::vdev::VirtualDevice::new(profile);
        let mut s = crate::session::SimSession::new(dev).with_da(vec![0xABu8; 4096]);
        s.load_da().unwrap();
        s.open_storage().unwrap();
        s.parse_gpt().unwrap();
        s
    }

    #[test]
    fn boot_write_detects_enabled_verification_via_vbmeta() {
        let profile = crate::transport::vdev::DeviceProfile::preset("v5-clean").unwrap();
        let mut s = ready_session(profile);
        match check(&mut s, "boot_a") {
            VerificationState::Enabled { source, .. } => {
                assert!(
                    source.contains("vbmeta_a"),
                    "应优先读同槽位 vbmeta，实际: {}",
                    source
                );
            }
            other => panic!("v5-clean 的 vbmeta flags=0，应为 Enabled: {:?}", other),
        }
    }

    #[test]
    fn falls_back_to_seccfg_when_no_vbmeta() {
        let mut profile = crate::transport::vdev::DeviceProfile::preset("v5-clean").unwrap();
        profile
            .partitions
            .retain(|(n, _, _)| !n.starts_with("vbmeta"));
        profile.seccfg_lock_state = 3;
        profile.seccfg_dm_verity = 0;
        let mut s = ready_session(profile);
        match check(&mut s, "boot_a") {
            VerificationState::Disabled { source } => {
                assert!(source.contains("lock_state=3"), "实际: {}", source)
            }
            other => panic!("解锁且 dm_verity=0 应判为 Disabled: {:?}", other),
        }
    }

    #[test]
    fn locked_seccfg_reports_enabled() {
        let mut profile = crate::transport::vdev::DeviceProfile::preset("v5-clean").unwrap();
        profile
            .partitions
            .retain(|(n, _, _)| !n.starts_with("vbmeta"));
        profile.seccfg_lock_state = 1;
        let mut s = ready_session(profile);
        assert!(matches!(
            check(&mut s, "system"),
            VerificationState::Enabled { .. }
        ));
    }

    #[test]
    fn vbmeta_candidates_prefers_slot() {
        assert_eq!(
            vbmeta_candidates("boot_a"),
            vec!["vbmeta_a", "vbmeta_system_a", "vbmeta", "vbmeta_system"]
        );
        assert_eq!(vbmeta_candidates("boot"), vec!["vbmeta", "vbmeta_system"]);
    }
}
