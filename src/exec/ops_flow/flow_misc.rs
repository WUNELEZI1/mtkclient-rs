use super::*;

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
