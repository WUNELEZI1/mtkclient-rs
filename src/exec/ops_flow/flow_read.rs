use super::*;

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
