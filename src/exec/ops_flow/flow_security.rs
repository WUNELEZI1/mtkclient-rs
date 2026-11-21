use super::*;

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
