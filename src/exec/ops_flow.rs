//! 端到端流程测试：read / write（含断点续传）/ erase / seccfg / vbmeta / unlock。
//!
//! 全部跑在 `--sim` 同款的虚拟设备上，因此验证的是**真机同构的执行路径**
//! （Executor → Session → da::engine → 设备侧状态机），而不是 mock 返回值。

use super::*;
use crate::ops::erase::EraseOp;
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
    // vdev 给每个分区写入可辨识的初始内容（seed + 递增），首个字节非零
    assert!(data.iter().take(4096).any(|b| *b != 0), "内容不应全零");
    // 目标文件不应残留 .part
    assert!(!dir.join("boot_a.img.part").exists());
}

#[test]
fn write_needs_confirmation_for_critical_partition() {
    let dir = tmpdir("confirm");
    let img = dir.join("boot.img");
    std::fs::write(&img, vec![0x5Au8; 4096]).unwrap();
    let mut s = sim("v5-clean");
    // 默认（非交互、无 --yes）：必须拒绝
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
    // 直接从设备读回比对（独立于 op 内部的校验）
    let span = s.find_partition("boot_a").unwrap();
    let got = s.read_flash(span.addr, payload.len() as u64).unwrap();
    assert_eq!(got, payload);
}

#[test]
fn write_rejects_image_larger_than_partition() {
    let dir = tmpdir("toobig");
    let img = dir.join("huge.img");
    // vbmeta_a 只有 0x8000，写 1MB 必须拒绝
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

#[test]
fn flaky_write_resumes_from_checkpoint_after_replug() {
    let dir = tmpdir("resume");
    let img = dir.join("big.img");
    // 20 MiB：超过 flaky-write 的 10 MiB 断点
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
    // checkpoint 已落盘
    assert!(
        crate::ops::checkpoint::WriteCheckpoint::exists(img.to_str().unwrap()),
        "应写出 .resume"
    );

    // 模拟拔插重连：flash 内容保留，故障清除
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

    // 设备内容与源文件完全一致（含第一次已写的 10MB）
    let span = s.find_partition("boot_b").unwrap();
    let got = s.read_flash(span.addr, payload.len() as u64).unwrap();
    assert_eq!(got, payload, "断点续传后内容必须与源文件一致");
    // 成功后 checkpoint 删除
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
    // 幂等：再解锁一次不报错且不重复写
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

    // 活动槽 A 的 flags 已改为 3，B 槽仍是 0
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
    // 未写入任何分区
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
    // seccfg 已解锁
    let span = s.find_partition("seccfg").unwrap();
    let data = s.read_flash(span.addr, 0x40).unwrap();
    assert_eq!(u32::from_le_bytes(data[0x0C..0x10].try_into().unwrap()), 3);
    assert_eq!(u32::from_le_bytes(data[0x10..0x14].try_into().unwrap()), 0);
    // vbmeta_a 已被关校验
    let a = s.find_partition("vbmeta_a").unwrap();
    let va = s.read_flash(a.addr, 0x100).unwrap();
    assert_eq!(u32::from_be_bytes(va[0x78..0x7C].try_into().unwrap()), 3);
    // vbmeta_b 未动（非活动槽 SKIP）
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
    // StorageWrite 在能力偏序里蕴含 StorageRead（写得进去必然读得出来），
    // 因此这里只能断言"没多要跟写无关的能力"。
    assert!(!write.contains(Capability::RpmbAccess));
    assert!(!write.contains(Capability::DaXml));
}
