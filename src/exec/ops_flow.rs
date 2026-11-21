//! End-to-end flow tests: read / write (including resume) / erase / seccfg / vbmeta / unlock.
//!
//! Everything runs on the same virtual device as `--sim`, so what is validated is the **real-device-isomorphic
//! execution path** (Executor → Session → da::engine → device-side state machine), not mock return values.

use super::*;
use crate::ops::erase::EraseOp;
use crate::ops::format::FormatOp;
use crate::ops::printgpt::PrintGptOp;
use crate::ops::read::ReadOp;
use crate::ops::seccfg::{SeccfgAction, SeccfgOp};
use crate::ops::unlock::UnlockOp;
use crate::ops::vbmeta::VbmetaOp;
use crate::ops::write::WriteOp;
use crate::session::SimSession;
use crate::transport::vdev::VirtualDevice;

fn sim(preset: &str) -> SimSession {
    let dev = VirtualDevice::from_preset(preset).expect("预设存在");
    SimSession::new(dev).with_da(vec![0xABu8; 4096])
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mtk-ops-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn exec_yes() -> Executor {
    Executor::new(Mode::Auto).with_assume_yes(true)
}

#[test]
fn read_partition_to_file() {
    let dir = tmpdir("read");
    let out = dir.join("boot_a.img");
    let mut s = sim("v5-clean");
    let r = exec_yes()
        .execute(
            &ReadOp {
                part: "boot_a".into(),
                out: out.to_str().unwrap().into(),
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert_eq!(r.outcome.facts[0].1, (32u64 * 1024 * 1024).to_string());
    let data = std::fs::read(&out).unwrap();
    // vdev writes recognizable initial content to each partition (seed + increment); the first byte is non-zero
    assert!(data.iter().take(4096).any(|b| *b != 0), "内容不应全零");
    // No .part should remain for the target file
    assert!(!dir.join("boot_a.img.part").exists());
}

#[test]
fn write_needs_confirmation_for_critical_partition() {
    let dir = tmpdir("confirm");
    let img = dir.join("boot.img");
    std::fs::write(&img, vec![0x5Au8; 4096]).unwrap();
    let mut s = sim("v5-clean");
    // Default (non-interactive, no --yes): must reject
    let err = Executor::new(Mode::Auto)
        .execute(
            &WriteOp {
                part: "boot_a".into(),
                file: img.to_str().unwrap().into(),
                verify: false,
            },
            &mut s,
            false,
            0,
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("需要确认"), "实际: {}", msg);
    assert!(msg.contains("--yes"), "提示里应给出解决办法: {}", msg);
}

#[test]
fn write_then_read_back_matches() {
    let dir = tmpdir("roundtrip");
    let img = dir.join("payload.img");
    let payload: Vec<u8> = (0..(512 * 64)).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&img, &payload).unwrap();

    let mut s = sim("v5-clean");
    let r = exec_yes()
        .execute(
            &WriteOp {
                part: "boot_a".into(),
                file: img.to_str().unwrap().into(),
                verify: true,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert_eq!(
        r.outcome
            .facts
            .iter()
            .find(|(k, _)| k == "verified")
            .map(|(_, v)| v.as_str()),
        Some("32768")
    );
    // Read back directly from the device and compare (independent of the op's internal verification)
    let span = s.find_partition("boot_a").unwrap();
    let got = s.read_flash(span.addr, payload.len() as u64).unwrap();
    assert_eq!(got, payload);
}

#[test]
fn write_rejects_image_larger_than_partition() {
    let dir = tmpdir("toobig");
    let img = dir.join("huge.img");
    // vbmeta_a is only 0x8000; writing 1MB must be rejected
    std::fs::write(&img, vec![0u8; 1024 * 1024]).unwrap();
    let mut s = sim("v5-clean");
    let err = exec_yes()
        .execute(
            &WriteOp {
                part: "vbmeta_a".into(),
                file: img.to_str().unwrap().into(),
                verify: false,
            },
            &mut s,
            false,
            0,
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝写入"), "实际: {}", err);
}

/// CVE-2022-20069 scenario: must select this path when affected and unpatched; must not plan it when patched.
#[test]
fn cve_20069_selected_only_when_unpatched() {
    // Affected and unpatched: BROM unreachable (no kamakiri2), the only viable path is Preloader OOB
    let dev = VirtualDevice::from_preset("cve-20069-vulnerable").unwrap();
    let mut da = vec![0u8; 4096];
    da[..9].copy_from_slice(b"MTK_DA_v5");
    let mut s = SimSession::new(dev).with_da(da);
    let r = Executor::new(Mode::Auto)
        .execute(&PrintGptOp, &mut s, false, 0)
        .unwrap();
    assert!(
        r.executed.iter().any(|x| x.contains("cve-20069")),
        "未修补设备必须走 CVE-2022-20069 路径: {:?}",
        r.executed
    );

    // Patched: probe reports unavailable, no viable plan overall (and the error names cve-20069).
    // DA1 also carries the Carbonara hardening signature so carbonara is not planned either — otherwise it
    // would become another path that "looks viable but is SLA-rejected only at execution time", masking the real conclusion.
    let dev = VirtualDevice::from_preset("cve-20069-patched").unwrap();
    let mut da = vec![0u8; 4096];
    da[..9].copy_from_slice(b"MTK_DA_v5");
    da[0x100..0x104].copy_from_slice(&0x4000_0000u32.to_le_bytes());
    let mut s = SimSession::new(dev).with_da(da);
    let err = Executor::new(Mode::Auto)
        .execute(&PrintGptOp, &mut s, false, 0)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cve-20069"), "实际: {}", err);
    assert!(err.contains("not-vulnerable"), "实际: {}", err);
}

/// carbonara-hardened scenario: DA1 already has the patch signature → probe returns NotVulnerable,
/// Planner must not put carbonara into any path (even if the device has SLA on and needs a bypass).
#[test]
fn carbonara_hardened_plan_never_selects_carbonara() {
    let dev = VirtualDevice::from_preset("carbonara-hardened").expect("预设存在");
    // Simulate a hardened DA1: carry the Carbonara V5 patch signature
    let mut da = vec![0xABu8; 4096];
    da[0x800..0x808].copy_from_slice(b"\x06\x9B\x4F\xF0\x80\x40\x02\xA9");
    let mut s = SimSession::new(dev).with_da(da);

    let r = Executor::new(Mode::Auto)
        .execute(&PrintGptOp, &mut s, false, 0)
        .unwrap();
    assert!(
        !r.executed.iter().any(|x| x.contains("carbonara")),
        "加固 DA1 下不得规划 carbonara: {:?}",
        r.executed
    );
    assert!(
        r.executed.iter().any(|x| x.contains("kamakiri2")),
        "SLA 设备仍应走 kamakiri2 bypass: {:?}",
        r.executed
    );
}

/// Phase 1.2: when the checkpoint prefix CRC does not match the device's actual content, it must **abort with an error**,
/// never continuing to write after bad data ("absolute defense").
#[test]
fn resume_rejects_corrupted_prefix() {
    use crate::ops::checkpoint::WriteCheckpoint;
    let dir = tmpdir("resume-bad");
    let img = dir.join("big.img");
    let payload: Vec<u8> = (0..(20 * 1024 * 1024)).map(|i| (i % 253) as u8).collect();
    std::fs::write(&img, &payload).unwrap();
    let file = img.to_str().unwrap().to_string();

    let mut s = sim("flaky-write");
    let _ = exec_yes()
        .execute(
            &WriteOp {
                part: "boot_b".into(),
                file: file.clone(),
                verify: false,
            },
            &mut s,
            false,
            0,
        )
        .unwrap_err();
    assert!(WriteCheckpoint::exists(&file), "应写出 .resume");

    // After reconnect, tamper with the already-written prefix (simulating "flashed by someone else / switched machines in between")
    s.replug();
    s.load_da().unwrap();
    s.open_storage().unwrap();
    let span = s.find_partition("boot_b").unwrap();
    s.write_flash(span.addr, &[0xFFu8; 512]).unwrap();

    let err = exec_yes()
        .execute(
            &WriteOp {
                part: "boot_b".into(),
                file: file.clone(),
                verify: true,
            },
            &mut s,
            false,
            0,
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("断点数据已损坏"), "实际: {}", err);
    assert!(
        WriteCheckpoint::exists(&file),
        "断点必须保留，由用户决定删除重写（不能替用户悄悄作废）"
    );
    WriteCheckpoint::clear(&file);
}

/// Phase 1.2: silent corruption caused by chaos bit flips must be caught by read-back verification, not passed off as success.
#[test]
fn chaos_bitflip_write_is_detected_by_verify() {
    use crate::transport::vdev::ChaosConfig;
    let dir = tmpdir("chaos-write");
    let img = dir.join("boot.img");
    let payload: Vec<u8> = (0..(4 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    std::fs::write(&img, &payload).unwrap();

    let cfg = ChaosConfig {
        seed: 5,
        // bitflip is decided once per "write block"; 20‰ is unlikely to hit within 4 blocks.
        // Use 500‰ here to guarantee a steady hit; this validates "can catch it", not "catches it occasionally".
        bitflip_permille: 500,
        ..ChaosConfig::default()
    };
    let dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    let mut s = SimSession::new(dev).with_da(vec![0xABu8; 4096]);

    let err = exec_yes()
        .execute(
            &WriteOp {
                part: "boot_a".into(),
                file: img.to_str().unwrap().into(),
                verify: true,
            },
            &mut s,
            false,
            0,
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("校验失败") || err.contains("断点"),
        "位翻转必须被回读校验发现，实际: {}",
        err
    );
}

#[test]
fn flaky_write_resumes_from_checkpoint_after_replug() {
    let dir = tmpdir("resume");
    let img = dir.join("big.img");
    // 20 MiB: exceeds flaky-write's 10 MiB checkpoint
    let payload: Vec<u8> = (0..(20 * 1024 * 1024)).map(|i| (i % 253) as u8).collect();
    std::fs::write(&img, &payload).unwrap();

    let mut s = sim("flaky-write");
    let first = exec_yes().execute(
        &WriteOp {
            part: "boot_b".into(),
            file: img.to_str().unwrap().into(),
            verify: false,
        },
        &mut s,
        false,
        0,
    );
    let err = first.unwrap_err().to_string();
    assert!(
        err.contains("断点") || err.contains("断开"),
        "应报告断点/断开: {}",
        err
    );
    // checkpoint has been persisted
    assert!(
        crate::ops::checkpoint::WriteCheckpoint::exists(img.to_str().unwrap()),
        "应写出 .resume"
    );

    // Simulate unplug/replug reconnect: flash content is retained, fault cleared
    s.replug();

    let second = exec_yes()
        .execute(
            &WriteOp {
                part: "boot_b".into(),
                file: img.to_str().unwrap().into(),
                verify: true,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    let resumed: u64 = second
        .outcome
        .facts
        .iter()
        .find(|(k, _)| k == "resumed_from")
        .map(|(_, v)| v.parse().unwrap())
        .unwrap();
    assert!(resumed > 0, "第二次必须从断点继续，而不是从头写");

    // Device content exactly matches the source file (including the 10MB written the first time)
    let span = s.find_partition("boot_b").unwrap();
    let got = s.read_flash(span.addr, payload.len() as u64).unwrap();
    assert_eq!(got, payload, "断点续传后内容必须与源文件一致");
    // checkpoint deleted after success
    assert!(!crate::ops::checkpoint::WriteCheckpoint::exists(
        img.to_str().unwrap()
    ));
}

#[test]
fn erase_is_idempotent_and_blanks_partition() {
    let mut s = sim("v5-clean");
    for round in 1..=2 {
        let r = exec_yes()
            .execute(
                &EraseOp {
                    part: "userdata".into(),
                    check: true,
                },
                &mut s,
                false,
                0,
            )
            .unwrap_or_else(|e| panic!("第 {} 次擦除失败: {}", round, e));
        assert!(r.outcome.summary.contains("已擦除"));
    }
    let span = s.find_partition("userdata").unwrap();
    let head = s.read_flash(span.addr, 4096).unwrap();
    assert!(head.iter().all(|b| *b == 0xFF), "擦除后应为 0xFF");
}

#[test]
fn seccfg_unlock_writes_dm_verity_zero() {
    let mut s = sim("v5-clean");
    let r = exec_yes()
        .execute(
            &SeccfgOp {
                action: SeccfgAction::Unlock,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert_eq!(
        r.outcome
            .facts
            .iter()
            .find(|(k, _)| k == "lock_state")
            .map(|(_, v)| v.as_str()),
        Some("3")
    );
    let span = s.find_partition("seccfg").unwrap();
    let data = s.read_flash(span.addr, 0x40).unwrap();
    assert_eq!(u32::from_le_bytes(data[0x0C..0x10].try_into().unwrap()), 3);
    assert_eq!(
        u32::from_le_bytes(data[0x10..0x14].try_into().unwrap()),
        0,
        "偏移 0x10 必须是 0（否则 dm-verity error）"
    );
    // Idempotent: unlocking again neither errors nor rewrites
    let again = exec_yes()
        .execute(
            &SeccfgOp {
                action: SeccfgAction::Unlock,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert!(again.outcome.summary.contains("无需修改"));
}

#[test]
fn format_partition_writes_ext4_and_f2fs_magic() {
    // The format op must first pass the simulation closed loop (write + read back magic) before it may touch real devices.
    for (fs, magic_off, magic_bytes) in [
        ("ext4", 0x438usize, vec![0x53, 0xEF]),
        ("f2fs", 0x400usize, vec![0x10, 0x20, 0xF5, 0xF2]),
    ] {
        let mut s = sim("v5-clean");
        let r = exec_yes()
            .execute(
                &FormatOp {
                    part: "userdata".into(),
                    fs: fs.into(),
                },
                &mut s,
                false,
                0,
            )
            .unwrap();
        assert_eq!(
            r.outcome
                .facts
                .iter()
                .find(|(k, _)| k == "fs")
                .map(|(_, v)| v.as_str()),
            Some(fs)
        );
        let span = s.find_partition("userdata").unwrap();
        let back = s.read_flash(span.addr, 0x43A).unwrap();
        assert_eq!(
            &back[magic_off..magic_off + magic_bytes.len()],
            magic_bytes.as_slice(),
            "{} magic 应正确落盘",
            fs
        );
    }
}

#[test]
fn seccfg_v3_unlock_and_lock_roundtrip() {
    // V3 seccfg uses software SEJ (different from V4's HACC path); full-version support must be verified end to end.
    let mut s = sim("v5-seccfg-v3");
    let r = exec_yes()
        .execute(
            &SeccfgOp {
                action: SeccfgAction::Unlock,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert_eq!(
        r.outcome
            .facts
            .iter()
            .find(|(k, _)| k == "seccfg_version")
            .map(|(_, v)| v.as_str()),
        Some("V3")
    );
    // Read back the image to confirm the V3 structure still parses and attr has switched to UNLOCK
    let span = s.find_partition("seccfg").unwrap();
    let data = s.read_flash(span.addr, span.size.min(0x4000)).unwrap();
    let v3 = crate::security::seccfg::v3::SecCfgV3::parse(&data).unwrap();
    assert_eq!(
        v3.seccfg_attr,
        crate::security::seccfg::v3::SecCfgV3::ATTR_UNLOCK
    );

    // Idempotent: unlocking again does not rewrite
    let again = exec_yes()
        .execute(
            &SeccfgOp {
                action: SeccfgAction::Unlock,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    assert!(again.outcome.summary.contains("无需修改"));

    // Lock again to return to ATTR_DEFAULT
    exec_yes()
        .execute(
            &SeccfgOp {
                action: SeccfgAction::Lock,
            },
            &mut s,
            false,
            0,
        )
        .unwrap();
    let data = s.read_flash(span.addr, span.size.min(0x4000)).unwrap();
    let v3 = crate::security::seccfg::v3::SecCfgV3::parse(&data).unwrap();
    assert_eq!(
        v3.seccfg_attr,
        crate::security::seccfg::v3::SecCfgV3::ATTR_DEFAULT
    );
}

#[test]
fn vbmeta_patches_only_active_slot() {
    let mut s = sim("ufs-ab");
    let r = exec_yes()
        .execute(&VbmetaOp { mode: 3 }, &mut s, false, 0)
        .unwrap();
    let patched = r
        .outcome
        .facts
        .iter()
        .find(|(k, _)| k == "patched")
        .map(|(_, v)| v.clone())
        .unwrap();
    let skipped = r
        .outcome
        .facts
        .iter()
        .find(|(k, _)| k == "skipped")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert!(patched.contains("vbmeta_a"), "实际: {}", patched);
    assert!(!patched.contains("vbmeta_b"), "不应改非活动槽: {}", patched);
    assert!(skipped.contains("vbmeta_b"), "应记录 SKIP: {}", skipped);
    assert!(
        skipped.contains("vbmeta_system_b"),
        "应记录 SKIP: {}",
        skipped
    );

    // Active slot A's flags have been changed to 3, slot B is still 0
    let a = s.find_partition("vbmeta_a").unwrap();
    let da = s.read_flash(a.addr, 0x100).unwrap();
    assert_eq!(u32::from_be_bytes(da[0x78..0x7C].try_into().unwrap()), 3);
    let b = s.find_partition("vbmeta_b").unwrap();
    let db = s.read_flash(b.addr, 0x100).unwrap();
    assert_eq!(u32::from_be_bytes(db[0x78..0x7C].try_into().unwrap()), 0);
}

#[test]
fn vbmeta_aborts_without_writing_when_slot_unknown() {
    let mut s = sim("ab-unknown-slot");
    let err = exec_yes()
        .execute(&VbmetaOp { mode: 3 }, &mut s, false, 0)
        .unwrap_err()
        .to_string();
    assert!(err.contains("无法确定活动 A/B 槽位"), "实际: {}", err);
    // No partition was written
    let a = s.find_partition("vbmeta_a").unwrap();
    let data = s.read_flash(a.addr, 0x100).unwrap();
    assert_eq!(
        u32::from_be_bytes(data[0x78..0x7C].try_into().unwrap()),
        0,
        "槽位未知时一个字节都不该改"
    );
}

#[test]
fn unlock_chain_does_seccfg_then_vbmeta() {
    let mut s = sim("v5-clean");
    let r = exec_yes().execute(&UnlockOp, &mut s, false, 0).unwrap();
    assert!(
        r.outcome.summary.contains("lock_state=3"),
        "实际: {}",
        r.outcome.summary
    );
    // seccfg unlocked
    let span = s.find_partition("seccfg").unwrap();
    let data = s.read_flash(span.addr, 0x40).unwrap();
    assert_eq!(u32::from_le_bytes(data[0x0C..0x10].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(data[0x10..0x14].try_into().unwrap()), 0);
    // vbmeta_a verification disabled
    let a = s.find_partition("vbmeta_a").unwrap();
    let va = s.read_flash(a.addr, 0x100).unwrap();
    assert_eq!(u32::from_be_bytes(va[0x78..0x7C].try_into().unwrap()), 3);
    // vbmeta_b untouched (SKIP for non-active slot)
    let b = s.find_partition("vbmeta_b").unwrap();
    let vb = s.read_flash(b.addr, 0x100).unwrap();
    assert_eq!(u32::from_be_bytes(vb[0x78..0x7C].try_into().unwrap()), 0);
}

#[test]
fn operations_declare_the_capabilities_they_use() {
    use crate::core::capability::Capability;
    let read = ReadOp {
        part: "boot_a".into(),
        out: "x".into(),
    }
    .required_caps();
    assert!(read.contains(Capability::StorageRead));
    assert!(!read.contains(Capability::StorageWrite));
    let write = WriteOp {
        part: "boot_a".into(),
        file: "x".into(),
        verify: false,
    }
    .required_caps();
    assert!(write.contains(Capability::StorageWrite));
    // StorageWrite implies StorageRead in the capability partial order (if you can write, you can read),
    // so here we can only assert "no extra capabilities unrelated to writing were requested".
    assert!(!write.contains(Capability::RpmbAccess));
    assert!(!write.contains(Capability::DaXml));
}
