//! Write-side checkpoint (resume) support.
//!
//! Design points:
//! - **Backend-agnostic**: the real device (`DAXFlash`) and the simulation (`da::engine`) share one checkpoint;
//! - **Strong evidence only**: resume only if the partition name, target address, total length, source file size and mtime, and **GPT hash** all
//!   match; any change restarts from scratch (better to rewrite than to write to the wrong offset);
//! - **One CRC per 64 MiB**: verify data already written on the device before resuming; abort immediately if the prefix is corrupt,
//!   never "keep writing past it" and leave bad data in the partition;
//! - **Persistence timing**: flush every 8 MiB and immediately on error; a killed process loses at most 8 MiB;
//! - **Delete on completion**: remove `.resume` on success to avoid an accidental resume next time.

use std::io::Write;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

/// Checkpoint version (incremented when the format changes; old files are invalidated automatically).
///
/// v1: offset only.
/// v2: added the GPT CRC and per-chunk CRC list (Phase 1.2).
const VERSION: u32 = 2;

/// Resume state file suffix.
pub const RESUME_SUFFIX: &str = ".resume";

/// Persistence interval.
pub const PERSIST_INTERVAL: u64 = 8 * 1024 * 1024;

/// Chunk size for per-chunk CRC (64 MiB).
pub const CRC_CHUNK: u64 = 64 * 1024 * 1024;

/// One CRC chunk of written data (covers `[offset, offset+len)`, relative to the partition start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkCrc {
    pub offset: u64,
    pub len: u64,
    /// CRC32-IEEE of the source data (including trailing zero padding).
    pub crc: u32,
}

/// Resume state of a single write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteCheckpoint {
    pub part: String,
    pub addr: u64,
    pub total: u64,
    pub written: u64,
    pub file_size: u64,
    pub file_mtime: u64,
    /// CRC32 of the device GPT at write time; a changed GPT means the partition layout may have changed.
    pub gpt_crc: u32,
    /// Chunk size (the value actually used at write time, for verification and forward compatibility).
    pub chunk_size: u64,
    /// CRC list of the written range, which must cover `[0, written)` without gaps.
    pub chunks: Vec<ChunkCrc>,
}

impl WriteCheckpoint {
    /// Path of `.resume` (in the same directory as the source file).
    pub fn path_for(file: &str) -> PathBuf {
        PathBuf::from(format!("{}{}", file, RESUME_SUFFIX))
    }

    /// Read and strictly validate.
    ///
    /// Return value semantics (central to the "never resume silently" rule):
    /// - `Ok(None)`: no usable checkpoint (file missing / fingerprint mismatch); write from scratch;
    /// - `Ok(Some(cp))`: checkpoint fully trusted; safe to resume;
    /// - `Err(msg)`: **checkpoint exists but is untrusted** (corrupt or GPT changed); abort and ask the user to handle it;
    ///   never silently treat it as "no checkpoint" and overwrite from the start — that would make the user think a resume happened.
    pub fn load_checked(
        file: &str,
        part: &str,
        addr: u64,
        total: u64,
        gpt_crc: Option<u32>,
    ) -> Result<Option<Self>, String> {
        let path = Self::path_for(file);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取续传状态失败 {}: {}", path.display(), e))?;
        let cp = Self::parse(&text).ok_or_else(|| {
            format!(
                "续传状态已损坏（{}），无法确认上次写到哪；请删除该文件后从头写入",
                path.display()
            )
        })?;
        let (size, mtime) = match file_signature(file) {
            Some(v) => v,
            None => return Ok(None),
        };
        // Fingerprint mismatch: this is another task's checkpoint, not corruption; just write from scratch.
        if cp.part != part
            || cp.addr != addr
            || cp.total != total
            || cp.file_size != size
            || cp.file_mtime != mtime
        {
            return Ok(None);
        }
        if let Some(want) = gpt_crc
            && cp.gpt_crc != want
        {
            return Err(format!(
                "分区表已变化（断点 GPT CRC 0x{:08X} ≠ 当前 0x{:08X}），地址可能已失效；\
                 请删除 {} 后从头写入",
                cp.gpt_crc,
                want,
                path.display()
            ));
        }
        // Structural self-check: the CRC list must cover [0, written) without gaps.
        if !cp.chunks_cover_written() {
            return Err(format!(
                "续传状态的 CRC 清单不完整（written={}，块数={}），无法确认已写数据；\
                 请删除 {} 后从头写入",
                cp.written,
                cp.chunks.len(),
                path.display()
            ));
        }
        if cp.written == 0 || cp.written >= total {
            return Ok(None);
        }
        Ok(Some(cp))
    }

    /// Parse `.resume` text (corrupt content is treated as invalid).
    pub fn parse(text: &str) -> Option<Self> {
        let mut version = 0u32;
        let mut part = String::new();
        let mut addr = 0u64;
        let mut total = 0u64;
        let mut written = 0u64;
        let mut file_size = 0u64;
        let mut file_mtime = 0u64;
        let mut gpt_crc = 0u32;
        let mut chunk_size = CRC_CHUNK;
        let mut chunks: Vec<ChunkCrc> = Vec::new();
        for line in text.lines() {
            let (k, v) = line.split_once('=')?;
            match k.trim() {
                "version" => version = v.trim().parse().ok()?,
                "part" => part = v.trim().to_string(),
                "addr" => addr = u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()?,
                "total" => total = v.trim().parse().ok()?,
                "written" => written = v.trim().parse().ok()?,
                "file_size" => file_size = v.trim().parse().ok()?,
                "file_mtime" => file_mtime = v.trim().parse().ok()?,
                "gpt_crc" => {
                    gpt_crc = u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()?
                }
                "chunk_size" => chunk_size = v.trim().parse().ok()?,
                "crc" => chunks.push(parse_chunk(v.trim())?),
                _ => {}
            }
        }
        if version != VERSION || part.is_empty() || total == 0 {
            return None;
        }
        Some(Self {
            part,
            addr,
            total,
            written,
            file_size,
            file_mtime,
            gpt_crc,
            chunk_size,
            chunks,
        })
    }

    /// Whether the CRC list covers `[0, written)` without gaps or overlaps.
    pub fn chunks_cover_written(&self) -> bool {
        let mut end = 0u64;
        for c in &self.chunks {
            if c.offset != end || c.len == 0 {
                return false;
            }
            end = match end.checked_add(c.len) {
                Some(v) => v,
                None => return false,
            };
        }
        end == self.written
    }

    /// Write out (atomic replace: write `.tmp` first, then rename).
    pub fn save(&self, file: &str) -> Result<(), String> {
        let path = Self::path_for(file);
        let tmp = path.with_extension("resume.tmp");
        let mut text = format!(
            "version={}\npart={}\naddr=0x{:X}\ntotal={}\nwritten={}\nfile_size={}\nfile_mtime={}\ngpt_crc=0x{:08X}\nchunk_size={}\n",
            VERSION,
            self.part,
            self.addr,
            self.total,
            self.written,
            self.file_size,
            self.file_mtime,
            self.gpt_crc,
            self.chunk_size
        );
        for c in &self.chunks {
            text.push_str(&format!("crc={}:{}:0x{:08X}\n", c.offset, c.len, c.crc));
        }
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| format!("写入续传状态失败 {}: {}", tmp.display(), e))?;
            f.write_all(text.as_bytes())
                .map_err(|e| format!("写入续传状态失败: {}", e))?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &path).map_err(|e| format!("提交续传状态失败: {}", e))
    }

    /// Delete the resume file (call on success).
    pub fn clear(file: &str) {
        let _ = std::fs::remove_file(Self::path_for(file));
    }

    #[cfg(test)]
    /// Whether it is still valid (for tests/diagnostics).
    pub fn exists(file: &str) -> bool {
        Path::new(&format!("{}{}", file, RESUME_SUFFIX)).exists()
    }
}

/// Parse `offset:len:0xCRC`.
fn parse_chunk(v: &str) -> Option<ChunkCrc> {
    let mut it = v.split(':');
    let offset = it.next()?.parse::<u64>().ok()?;
    let len = it.next()?.parse::<u64>().ok()?;
    let crc = u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
    Some(ChunkCrc { offset, len, crc })
}

/// Incremental CRC32 (IEEE 802.3, matching the GPT checksum).
///
/// Uses a lookup table: a bitwise implementation would need about 500 million iterations for 64 MiB, too slow.
#[derive(Debug, Clone)]
pub struct Crc32 {
    state: u32,
}

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_crc_table();

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Continue accumulating from a completed CRC value (to keep hashing the same chunk on resume).
    pub fn from_crc(crc: u32) -> Self {
        Self { state: !crc }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        for &b in data {
            let idx = ((crc ^ b as u32) & 0xFF) as usize;
            crc = (crc >> 8) ^ CRC_TABLE[idx];
        }
        self.state = crc;
    }

    pub fn finalize(&self) -> u32 {
        !self.state
    }
}

/// One-shot CRC32 calculation.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

/// Source file "fingerprint": size + mtime in seconds.
pub fn file_signature(file: &str) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(file).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

/// Per-chunk CRC tracker: splits data by `chunk_size` and accumulates incrementally as writes proceed.
///
/// On resume, use [`CrcTracker::resume`] to continue from the existing list and avoid re-reading written data.
#[derive(Debug, Clone)]
pub struct CrcTracker {
    chunk_size: u64,
    chunks: Vec<ChunkCrc>,
    cur_off: u64,
    cur_len: u64,
    hasher: Crc32,
}

impl CrcTracker {
    pub fn new(chunk_size: u64) -> Self {
        Self {
            chunk_size: chunk_size.max(1),
            chunks: Vec::new(),
            cur_off: 0,
            cur_len: 0,
            hasher: Crc32::new(),
        }
    }

    /// Restore from an existing list (if the last chunk is a partial, unfilled chunk, it becomes the current chunk and keeps accumulating).
    pub fn resume(chunk_size: u64, mut chunks: Vec<ChunkCrc>) -> Self {
        let chunk_size = chunk_size.max(1);
        let (cur_off, cur_len, hasher) = match chunks.last().copied() {
            Some(last) if last.len < chunk_size => {
                // Remove the partial chunk from the list, otherwise snapshot() would count the same chunk twice.
                chunks.pop();
                (last.offset, last.len, Crc32::from_crc(last.crc))
            }
            Some(last) => (last.offset + last.len, 0, Crc32::new()),
            None => (0, 0, Crc32::new()),
        };
        Self {
            chunk_size,
            chunks,
            cur_off,
            cur_len,
            hasher,
        }
    }

    /// Feed a segment of data already written to the device (including zero padding).
    pub fn feed(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let room = (self.chunk_size - self.cur_len) as usize;
            let take = room.min(data.len());
            self.hasher.update(&data[..take]);
            self.cur_len += take as u64;
            data = &data[take..];
            if self.cur_len == self.chunk_size {
                let crc = self.hasher.finalize();
                self.chunks.push(ChunkCrc {
                    offset: self.cur_off,
                    len: self.chunk_size,
                    crc,
                });
                self.cur_off += self.chunk_size;
                self.cur_len = 0;
                self.hasher = Crc32::new();
            }
        }
    }

    /// Number of bytes accumulated so far (including the current partially filled chunk).
    pub fn fed(&self) -> u64 {
        self.cur_off + self.cur_len
    }

    /// Append `[fed(), to)` from the source file into the list.
    ///
    /// Called deliberately only after the device confirms a successful write: bytes in the pre-read stage may never actually
    /// reach storage if the connection drops, and counting them early would make the list longer than the actual write, so resume-time verification would always fail.
    pub fn sync_from_file(&mut self, path: &str, to: u64) -> Result<(), String> {
        use std::io::{Read, Seek};
        let from = self.fed();
        if to <= from {
            return Ok(());
        }
        let mut f = std::fs::File::open(path).map_err(|e| format!("读取源文件失败: {}", e))?;
        f.seek(std::io::SeekFrom::Start(from))
            .map_err(|e| format!("seek 源文件失败: {}", e))?;
        let mut left = to - from;
        let mut buf = vec![0u8; 0x10_0000];
        while left > 0 {
            let want = (left as usize).min(buf.len());
            let mut n = 0usize;
            while n < want {
                let r = f
                    .read(&mut buf[n..want])
                    .map_err(|e| format!("读取源文件失败: {}", e))?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n < want {
                // Source file ended early: pad with zeros, consistent with the write path.
                buf[n..want].fill(0);
            }
            self.feed(&buf[..want]);
            left -= want as u64;
        }
        Ok(())
    }

    /// Current snapshot (including the partially filled chunk).
    pub fn snapshot(&self) -> Vec<ChunkCrc> {
        let mut out = self.chunks.clone();
        if self.cur_len > 0 {
            out.push(ChunkCrc {
                offset: self.cur_off,
                len: self.cur_len,
                crc: self.hasher.finalize(),
            });
        }
        out
    }

    #[cfg(test)]
    /// Number of completed chunks (for diagnostics).
    pub fn completed_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Chunk size used at write time.
    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let err = WriteCheckpoint::load_checked(&f, "boot_a", 0x100000, 2048, Some(0xDEAD_BEEF))
            .unwrap_err();
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
}
