use super::*;
use std::path::{Path, PathBuf};

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "mtk-ck-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cp(dir: &Path, written: u64, chunks: Vec<ChunkCrc>) -> (String, WriteCheckpoint) {
    let file = dir.join("boot.img");
    std::fs::write(&file, vec![7u8; 2048]).unwrap();
    let f = file.to_str().unwrap().to_string();
    let (size, mtime) = file_signature(&f).unwrap();
    let cp = WriteCheckpoint {
        part: "boot_a".into(),
        addr: 0x100000,
        total: 2048,
        written,
        file_size: size,
        file_mtime: mtime,
        gpt_crc: 0xDEAD_BEEF,
        chunk_size: 1024,
        chunks,
    };
    (f, cp)
}

#[test]
fn roundtrip_with_chunks() {
    let dir = tmpdir();
    let (f, cp) = cp(
        &dir,
        1536,
        vec![
            ChunkCrc {
                offset: 0,
                len: 1024,
                crc: 0x1111_1111,
            },
            ChunkCrc {
                offset: 1024,
                len: 512,
                crc: 0x2222_2222,
            },
        ],
    );
    cp.save(&f).unwrap();
    let got = WriteCheckpoint::load_checked(&f, "boot_a", 0x100000, 2048, Some(0xDEAD_BEEF))
        .unwrap()
        .unwrap();
    assert_eq!(got, cp);
    assert!(got.chunks_cover_written());
    WriteCheckpoint::clear(&f);
    assert!(!WriteCheckpoint::exists(&f));
}

#[test]
fn gpt_change_invalidates_checkpoint() {
    let dir = tmpdir();
    let (f, cp) = cp(
        &dir,
        1024,
        vec![ChunkCrc {
            offset: 0,
            len: 1024,
            crc: 0x1,
        }],
    );
    cp.save(&f).unwrap();
    let err =
        WriteCheckpoint::load_checked(&f, "boot_a", 0x100000, 2048, Some(0xBEEF)).unwrap_err();
    assert!(err.contains("分区表已变化"), "实际: {}", err);
    WriteCheckpoint::clear(&f);
}

#[test]
fn missing_gap_in_chunks_is_rejected() {
    let dir = tmpdir();
    let (f, cp) = cp(
        &dir,
        1536,
        vec![ChunkCrc {
            offset: 0,
            len: 1024,
            crc: 0x1,
        }],
    );
    cp.save(&f).unwrap();
    let err =
        WriteCheckpoint::load_checked(&f, "boot_a", 0x100000, 2048, Some(0xDEAD_BEEF)).unwrap_err();
    assert!(err.contains("CRC 清单不完整"), "实际: {}", err);
    WriteCheckpoint::clear(&f);
}

#[test]
fn corrupt_checkpoint_is_rejected_not_ignored() {
    let dir = tmpdir();
    let file = dir.join("x.img");
    std::fs::write(&file, vec![1u8; 100]).unwrap();
    let f = file.to_str().unwrap().to_string();
    std::fs::write(format!("{}.resume", f), "garbage").unwrap();
    assert!(WriteCheckpoint::parse("garbage").is_none());
    assert!(
        WriteCheckpoint::load_checked(&f, "boot_a", 0, 100, None).is_err(),
        "损坏的断点必须报错，不能静默当成没有断点"
    );
    WriteCheckpoint::clear(&f);
}

#[test]
fn tracker_splits_chunks_and_matches_oneshot_crc() {
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let mut t = CrcTracker::new(1024);
    t.feed(&data[..1500]);
    t.feed(&data[1500..]);
    assert_eq!(t.completed_chunks(), 2, "末块未满，不应计入已完成块");
    let snap = t.snapshot();
    assert_eq!(snap.len(), 3);
    assert_eq!(snap[0].len, 1024);
    assert_eq!(snap[1].len, 1024);
    assert_eq!(snap[2].len, 3000 - 2048);
    assert_eq!(snap[0].crc, crc32(&data[..1024]));
    assert_eq!(snap[1].crc, crc32(&data[1024..2048]));
    assert_eq!(snap[2].crc, crc32(&data[2048..]));
    assert_eq!(crc32(&data), {
        let mut c = Crc32::new();
        c.update(&data);
        c.finalize()
    });
}

#[test]
fn tracker_resume_continues_partial_chunk() {
    let data: Vec<u8> = (0..2000u32).map(|i| (i % 97) as u8).collect();
    // Last write ended at 1500 (partial chunk)
    let mut t1 = CrcTracker::new(1024);
    t1.feed(&data[..1500]);
    let saved = t1.snapshot();
    // Resume: start at 1500 and keep hashing the same chunk
    let mut t2 = CrcTracker::resume(1024, saved.clone());
    t2.feed(&data[1500..]);
    let merged = t2.snapshot();
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].crc, crc32(&data[..1024]));
    assert_eq!(merged[1].crc, crc32(&data[1024..]));
}

#[test]
fn crc_table_matches_bitwise_reference() {
    // Cross-check against the bitwise implementation in gpt.rs to ensure the lookup table is correct.
    let data = b"123456789";
    fn bitwise(d: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in d {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    assert_eq!(crc32(data), bitwise(data));
    assert_eq!(
        crc32(data),
        0xCBF4_3926,
        "标准校验向量 123456789 → 0xCBF43926"
    );
}
