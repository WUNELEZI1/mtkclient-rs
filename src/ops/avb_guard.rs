//! Post-write AVB verification status check (Phase 1.3).
//!
//! After flashing partitions that participate in AVB verification (such as `boot` /
//! `system`), if device verification is still enabled the boot will fail: at best it
//! drops to fastboot, at worst it boot-loops ("bricked after flashing"). This module
//! **forces** one verification status check after a successful write and prints a red
//! warning with remediation commands when verification is enabled.
//!
//! Decision priority (use whatever is available, earlier is more authoritative):
//! 1. flags of the same-slot/global `vbmeta` (`HASHTREE_DISABLED` / `VERIFICATION_DISABLED`);
//! 2. `seccfg` `lock_state` and `dm_verity_state`;
//! 3. none available -> `Unknown`: log a warning only, never block (a flashing failure
//!    must never be caused by "cannot determine").

use log::{info, warn};

use crate::color::Colorize;
use crate::security::avb::{FLAG_HASHTREE_DISABLED, FLAG_VERIFICATION_DISABLED, VbmetaImage};
use crate::security::seccfg::v4::SecCfgV4;
use crate::session::Session;

/// Verification state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationState {
    /// Verification disabled (safe).
    Disabled { source: String },
    /// Verification still enabled (flashing a modified image may brick the device).
    Enabled { source: String, detail: String },
    /// Cannot be determined.
    Unknown { reason: String },
}

/// Partitions that participate in AVB verification and may affect boot when modified
/// (compared after stripping the slot suffix).
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

/// Strips the `_a` / `_b` slot suffix.
fn base_name(part: &str) -> &str {
    match part.rsplit_once('_') {
        Some((base, "a" | "b")) => base,
        _ => part,
    }
}

/// Whether this partition needs an AVB check after being written.
///
/// `vbmeta*` itself is excluded: writing it is usually the "disable verification"
/// action, so it must not trigger a warning.
pub fn is_avb_protected(part: &str) -> bool {
    let base = base_name(part);
    if base.starts_with("vbmeta") {
        return false;
    }
    AVB_PARTITIONS.contains(&base)
}

/// Candidate vbmeta partitions (in priority order).
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

/// Checks the device's current AVB verification state.
pub fn check(session: &mut dyn Session, part: &str) -> VerificationState {
    // 1) vbmeta flags
    for name in vbmeta_candidates(part) {
        // Missing partition: try the next candidate (`find_partition` returns Err for missing partitions).
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
        // Parse failure is not a conclusion; try the next candidate / fall back to seccfg.
    }

    // 2) seccfg fallback
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

/// Called after a successful write: prints a red warning when verification is not
/// disabled (does not block the write).
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
