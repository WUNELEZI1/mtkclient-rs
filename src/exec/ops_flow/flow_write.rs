use super::*;

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
