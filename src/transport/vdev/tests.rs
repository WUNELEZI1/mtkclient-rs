use super::chaos::{ChaosRng, ChaosState};
use super::image::build_image;
use super::*;
use crate::partition::GptInfo;

fn handshake(dev: &mut VirtualDevice) {
    for b in wire::BROM_HANDSHAKE {
        dev.write(&[b]).unwrap();
        let mut echo = [0u8; 1];
        dev.read_exact(&mut echo).unwrap();
        assert_eq!(echo[0], !b);
    }
}

#[test]
fn handshake_echoes_inverted_bytes() {
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    handshake(&mut dev);
    assert!(dev.events.iter().any(|e| e.contains("handshake: ok")));
}

#[test]
fn hw_code_and_target_config() {
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    handshake(&mut dev);
    dev.write(&[wire::BROM_GET_HW_CODE]).unwrap();
    let mut buf = [0u8; 5];
    dev.read_exact(&mut buf).unwrap();
    assert_eq!(buf[0], wire::BROM_GET_HW_CODE);
    assert_eq!(u16::from_be_bytes([buf[1], buf[2]]), 0x0707);

    dev.write(&[wire::BROM_GET_TARGET_CONFIG]).unwrap();
    let mut buf = [0u8; 7];
    dev.read_exact(&mut buf).unwrap();
    assert_eq!(u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]), 0);
}

#[test]
fn gpt_image_parses_with_correct_crc() {
    let p = DeviceProfile::preset("v5-clean").unwrap();
    let img = build_image(&p);
    let info = GptInfo::parse(&img).expect("GPT 可解析");
    let report = info.crc_report().expect("CRC 可计算");
    assert!(report.header_ok(), "头 CRC 必须正确");
    assert!(report.partition_entries_ok(), "分区项 CRC 必须正确");
    let names: Vec<String> = info.partitions().iter().map(|e| e.name.clone()).collect();
    assert!(
        names.iter().any(|name| name == "super"),
        "实际: {:?}",
        names
    );
}

#[test]
fn emmc_info_reports_user_at_0x40() {
    let dev = VirtualDevice::from_preset("v5-clean").unwrap();
    let p = dev.emmc_info_payload();
    assert_eq!(p.len(), 104);
    assert_eq!(
        u64::from_le_bytes(p[0x40..0x48].try_into().unwrap()),
        dev.profile.storage.user
    );
}

#[test]
fn write_with_bad_checksum_is_rejected() {
    use crate::da::engine::DaEngine;
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    let mut e = DaEngine::new();
    e.connect(&mut dev).unwrap();
    e.load_da(&mut dev, &[0xAB; 256]).unwrap();
    e.begin_write(&mut dev, 0x100000, 512, 1, 8).unwrap();
    // Directly construct a chunk with a bad checksum
    let data = [0x11u8; 512];
    let bad = 0x1234u32.to_le_bytes();
    let zero = 0u32.to_le_bytes();
    let hdr = |len: usize| {
        let mut h = Vec::new();
        h.extend_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        h.extend_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        h.extend_from_slice(&(len as u32).to_le_bytes());
        h
    };
    let mut pkt = Vec::new();
    pkt.extend(hdr(4));
    pkt.extend(zero);
    pkt.extend(hdr(4));
    pkt.extend(bad);
    pkt.extend(hdr(data.len()));
    pkt.extend(data);
    dev.write(&pkt).unwrap();
    let mut st = [0u8; 16];
    let n = dev.read(&mut st).unwrap();
    // Response: 12-byte header + 4-byte status
    let status = u32::from_le_bytes(st[n - 4..n].try_into().unwrap());
    assert_eq!(status, wire::XFLASH_STATUS_CHECKSUM);
    assert!(
        dev.events.iter().any(|e| e.contains("checksum 不匹配")),
        "设备应记录校验和错误: {:?}",
        dev.events
    );
}

#[test]
fn erase_sets_erased_range_without_materializing_pages() {
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    let before = dev.pages.len();
    dev.erase_range(0, 64 * 1024 * 1024);
    assert!(dev.pages.len() <= before, "擦除不应物化页");
    let head = dev.read_window(0, 512);
    assert!(head.iter().all(|b| *b == 0xFF));
    // Idempotent: erasing again yields the same result
    dev.erase_range(0, 64 * 1024 * 1024);
    assert!(dev.read_window(0, 512).iter().all(|b| *b == 0xFF));
}

#[test]
fn cve_20435_alias_maps_to_cve_20069_fields() {
    // The historically mistaken ID is a compatibility alias: parsing must land on the cve_20069_* fields.
    let p = DeviceProfile::from_text("cve_20435.affected=true\ncve_20435.patched=false\n").unwrap();
    assert!(p.cve_20069_affected);
    assert!(!p.cve_20069_patched);

    let p2 = DeviceProfile::from_text("cve_20069.patched=true\n").unwrap();
    assert!(p2.cve_20069_patched);
}

#[test]
fn profile_text_roundtrip() {
    let text = "\
name=MT6765
hw_code=0x0707
da_code=0x6765
proto=v6
safety=0x6
sla=true
storage.user=0x10000000
partition=boot:34:0x2000000
fault=silent:512
exploit=kamakiri2
";
    let p = DeviceProfile::from_text(text).unwrap();
    assert_eq!(p.name, "MT6765");
    assert!(p.v6);
    assert!(p.sla_required);
    assert_eq!(p.safety, 6);
    assert_eq!(p.partitions.len(), 1);
    assert_eq!(p.faults, vec![(512, DeviceFault::Silent)]);
    assert!(DeviceProfile::preset("nope").is_none());
}

#[test]
fn fault_injection_turns_silent_then_disconnects() {
    let mut p = DeviceProfile::preset("v5-clean").unwrap();
    p.faults = vec![(2, DeviceFault::Silent)];
    let mut dev = VirtualDevice::new(p);
    dev.write(&[0xA0, 0x0A, 0x50, 0x05]).unwrap();
    let mut b = [0u8; 1];
    // Latched silence = replug required, reported as "disconnected" (not retryable)
    assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
}

#[test]
fn unknown_brom_command_is_recorded_not_fatal() {
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    handshake(&mut dev);
    dev.write(&[0x7E]).unwrap();
    assert!(dev.events.iter().any(|e| e.contains("未支持命令")));
}

/// The state file read/write layout must be strictly symmetric — cross-process resume depends entirely on it.
#[test]
fn state_roundtrip_preserves_pages_and_erased() {
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    dev.write_range(0x1000, &[0xAB; 8192]); // two pages
    dev.write_range(0x9000, &[0xCD; 4096]); // one page
    dev.erase_range(0x2_0000, 0x1_0000);
    dev.total_written = 0x1234;

    let path = std::env::temp_dir().join(format!("vdev-state-{}.bin", std::process::id()));
    let path = path.to_string_lossy().to_string();
    dev.save_state(&path).unwrap();

    let mut dst = VirtualDevice::from_preset("v5-clean").unwrap();
    dst.load_state(&path).unwrap();
    let _ = std::fs::remove_file(&path);

    assert_eq!(dst.pages, dev.pages); // even the preset built-in seed pages must match exactly
    assert_eq!(dst.total_written, 0x1234);
    assert_eq!(dst.erased, dev.erased);
    assert!(
        dev.erased
            .iter()
            .any(|(s, e)| *s <= 0x2_0000 && *e >= 0x3_0000)
    );
    // Content identical byte-for-byte (including untouched blank parts within pages)
    assert_eq!(dst.read_window(0x1000, 8192), vec![0xAB; 8192]);
    assert_eq!(dst.read_window(0x9000, 4096), vec![0xCD; 4096]);
    assert_eq!(dst.read_window(0x2_0000, 4096), vec![0xFF; 4096]);
}

#[test]
fn state_load_rejects_bad_magic_and_version() {
    let dir = std::env::temp_dir();
    let bad = dir.join(format!("vdev-bad-{}.bin", std::process::id()));
    let bad = bad.to_string_lossy().to_string();

    std::fs::write(&bad, b"nope\0\0\0\0\0\0\0\0").unwrap();
    let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
    assert!(dev.load_state(&bad).unwrap_err().contains("格式不匹配"));

    let mut v2 = Vec::from(*STATE_MAGIC);
    v2.extend_from_slice(&99u32.to_le_bytes());
    std::fs::write(&bad, &v2).unwrap();
    assert!(dev.load_state(&bad).unwrap_err().contains("版本不支持"));

    std::fs::write(&bad, &v2[..10]).unwrap();
    assert!(dev.load_state(&bad).unwrap_err().contains("格式不匹配"));
    let _ = std::fs::remove_file(&bad);
}

// ── Phase 1.1 Chaos injection ─────────────────────────────────────────

fn chaos_state(cfg: ChaosConfig) -> ChaosState {
    ChaosState {
        cfg,
        rng: ChaosRng::new(cfg.seed),
        bitflips: 0,
        drops: 0,
        silences: 0,
        glitches: 0,
    }
}

#[test]
fn chaos_rng_is_deterministic() {
    let mut a = ChaosRng::new(12345);
    let mut b = ChaosRng::new(12345);
    for _ in 0..16 {
        assert_eq!(a.next_u64(), b.next_u64(), "同种子必须给出同一序列");
    }
    let mut c = ChaosRng::new(54321);
    assert_ne!(a.next_u64(), c.next_u64(), "不同种子应不同");
}

#[test]
fn chaos_default_is_noop() {
    assert!(ChaosConfig::default().is_noop());
    let dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(ChaosConfig::default());
    assert!(dev.chaos.is_none(), "noop 配置不应启用混沌");
}

#[test]
fn chaos_bitflip_primitive() {
    // 0‰ never flips
    let mut nil = chaos_state(ChaosConfig::default());
    assert_eq!(nil.dirty(64), None);

    // 1000‰ always flips, with index/bit within range
    let cfg = ChaosConfig {
        seed: 42,
        bitflip_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut c = chaos_state(cfg);
    let (idx, bit) = c.dirty(64).expect("1000‰ 必须命中");
    assert!(idx < 64 && bit < 8, "翻转位置越界: ({}, {})", idx, bit);
    assert_eq!(c.bitflips, 1);
}

#[test]
fn chaos_silence_latches_read() {
    let cfg = ChaosConfig {
        seed: 1,
        silence_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    dev.out.push_back(0x5A);
    let mut b = [0u8; 1];
    // Latched silence = replug required, reported as Disconnected (not retryable, fail immediately)
    assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
    assert!(dev.silent, "静默必须锁存（直到 replug）");
    assert_eq!(dev.read(&mut b), Err(PortError::Disconnected));
    // Recover after replug (and calm the environment to verify replug clears the latch flag, not the chaos config itself)
    dev.replug();
    dev.chaos.as_mut().unwrap().cfg.silence_permille = 0;
    dev.out.push_back(0x5A);
    let n = dev.read(&mut b).unwrap();
    assert_eq!((n, b[0]), (1, 0x5A), "重连后应恢复出字节");
}

#[test]
fn chaos_glitch_is_transient_not_latched() {
    let cfg = ChaosConfig {
        seed: 1,
        glitch_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    dev.out.push_back(0x5A);
    let mut b = [0u8; 1];
    assert_eq!(dev.read(&mut b), Err(PortError::Timeout));
    assert!(!dev.silent, "瞬时超时不得锁存");
    assert_eq!(dev.chaos_counts().3, 1);
}

/// Phase 1.1b acceptance: real silence (link down) must fail **immediately**, never retry and wait in vain on a dead device.
#[test]
fn chaos_silence_fails_fast_without_retry() {
    use crate::da::engine::{DaEngine, EngineError};
    let cfg = ChaosConfig {
        seed: 1,
        silence_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    let mut e = DaEngine::new();
    let start = std::time::Instant::now();
    let err = e.connect(&mut dev).unwrap_err();
    assert!(
        matches!(err, EngineError::Port(PortError::Disconnected)),
        "静默应上报断开，实际 {:?}",
        err
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "静默必须立刻失败（不得退避重试）"
    );
}

/// Phase 1.1b acceptance: when a data block loses bytes (length mismatch), recover by clearing the input buffer + resending the
/// whole READ_DATA command — byte-level resume cannot recover lost bytes.
#[test]
fn chaos_drop_recovers_via_read_command_retry() {
    use crate::da::engine::DaEngine;
    let cfg = ChaosConfig {
        seed: 3,
        drop_permille: 60,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    let mut e = DaEngine::new();
    e.connect(&mut dev).unwrap();
    e.load_da(&mut dev, &[0xAB; 256]).unwrap();
    let data = e
        .read_flash(&mut dev, 0, 32768)
        .expect("丢包后应通过整条 READ_DATA 重试恢复");
    assert_eq!(data.len(), 32768);
    assert!(dev.chaos_counts().1 > 0, "本用例必须真的注入过丢包");
}

/// Phase 1.1b acceptance: chaos transient timeouts are absorbed by bounded retries; DA loading must not be interrupted by a single glitch.
#[test]
fn chaos_glitch_is_absorbed_by_bounded_retry() {
    use crate::da::engine::DaEngine;
    let cfg = ChaosConfig {
        seed: 7,
        glitch_permille: 30,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    let mut e = DaEngine::new();
    e.connect(&mut dev).expect("握手应能扛住瞬时超时");
    e.load_da(&mut dev, &[0xAB; 256])
        .expect("DA 加载应能扛住瞬时超时");
    assert!(
        dev.chaos_counts().3 > 0,
        "本用例必须真的注入过瞬时超时，否则测不到重试"
    );
}

#[test]
fn chaos_drop_truncates_read_and_loses_tail() {
    let cfg = ChaosConfig {
        seed: 3,
        drop_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    for i in 0..16u8 {
        dev.out.push_back(i);
    }
    let mut buf = [0u8; 16];
    let n = dev.read(&mut buf).unwrap();
    assert!(n >= 1 && n < 16, "丢包应截断返回，实际 {}", n);
    assert_eq!(dev.out.len(), 0, "尾部被丢的字节应真的丢弃（真丢包）");
    assert!(buf[..n].iter().enumerate().all(|(i, b)| *b == i as u8));
}

#[test]
fn chaos_off_keeps_read_intact() {
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(ChaosConfig::default());
    for i in 0..8u8 {
        dev.out.push_back(i);
    }
    let mut buf = [0u8; 8];
    let n = dev.read(&mut buf).unwrap();
    assert_eq!(n, 8);
    assert_eq!(buf, [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(dev.chaos_counts(), (0, 0, 0, 0));
}

/// End-to-end: the bit flip happens after the checksum passes, corrupting data on disk,
/// detectable only through host-side read-back verification — proving "silent corruption never passes as success".
#[test]
fn chaos_bitflip_corrupts_after_checksum_accepted() {
    use crate::da::engine::DaEngine;
    let cfg = ChaosConfig {
        seed: 9,
        bitflip_permille: 1000,
        ..ChaosConfig::default()
    };
    let mut dev = VirtualDevice::from_preset("v5-clean")
        .unwrap()
        .with_chaos(cfg);
    let mut e = DaEngine::new();
    e.connect(&mut dev).unwrap();
    e.load_da(&mut dev, &[0xAB; 256]).unwrap();
    e.begin_write(&mut dev, 0x100000, 512, 1, 8).unwrap();

    let data = [0x11u8; 512];
    let sum = (data.iter().map(|b| *b as u32).sum::<u32>()) & 0xFFFF;
    let hdr = |len: usize| {
        let mut h = Vec::new();
        h.extend_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        h.extend_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        h.extend_from_slice(&(len as u32).to_le_bytes());
        h
    };
    let mut pkt = Vec::new();
    pkt.extend(hdr(4));
    pkt.extend(0u32.to_le_bytes());
    pkt.extend(hdr(4));
    pkt.extend(sum.to_le_bytes());
    pkt.extend(hdr(data.len()));
    pkt.extend(data);
    dev.write(&pkt).unwrap();

    // The checksum itself passes (device returns status=0); the on-disk bit flip happens afterwards.
    let mut st = [0u8; 64];
    let n = dev.read(&mut st).unwrap();
    let status = u32::from_le_bytes(st[n - 4..n].try_into().unwrap());
    assert_eq!(status, 0, "校验和应被接受（翻转在之后）");

    let back = dev.read_window(0x100000, 512);
    assert_ne!(back, data.to_vec(), "位翻转必须让落盘数据与写入不一致");
    assert_eq!(dev.chaos_counts().0, 1, "应恰好翻转一次");
}
