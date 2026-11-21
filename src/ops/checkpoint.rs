//! 写入断点续传（checkpoint）。
//!
//! 设计要点：
//! - **与后端无关**：真机（`DAXFlash`）与仿真（`da::engine`）共用同一份 checkpoint；
//! - **只认强证据**：分区名、目标地址、总长度、源文件大小与 mtime、**GPT 哈希**全部
//!   匹配才续传，任何一项变化都从头开始（宁可重写，不要写错位）；
//! - **每 64 MiB 一个 CRC**：续传前先校验设备上已写数据，发现前缀损坏立即报错中止，
//!   绝不"接着往后写"把坏数据留在分区里；
//! - **写盘时机**：每 8 MiB 落一次、出错时立刻落一次，进程被杀最多丢 8 MiB；
//! - **完成后删除**：成功后删除 `.resume`，避免下次误续传。

use std::io::Write;
use std::path::{Path, PathBuf};

/// checkpoint 版本（格式变更时递增，旧文件自动失效）。
///
/// v1：只有偏移量。
/// v2：新增 GPT CRC 与分块 CRC 清单（Phase 1.2）。
const VERSION: u32 = 2;

/// 续传状态文件后缀。
pub const RESUME_SUFFIX: &str = ".resume";

/// 落盘间隔。
pub const PERSIST_INTERVAL: u64 = 8 * 1024 * 1024;

/// 分块 CRC 的块大小（64 MiB）。
pub const CRC_CHUNK: u64 = 64 * 1024 * 1024;

/// 已写数据的一个 CRC 分块（覆盖 `[offset, offset+len)`，相对分区起点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkCrc {
    pub offset: u64,
    pub len: u64,
    /// 源数据（含尾部补零）的 CRC32-IEEE。
    pub crc: u32,
}

/// 一次写入的续传状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteCheckpoint {
    pub part: String,
    pub addr: u64,
    pub total: u64,
    pub written: u64,
    pub file_size: u64,
    pub file_mtime: u64,
    /// 写入时设备 GPT 的 CRC32；GPT 变了说明分区布局可能已变。
    pub gpt_crc: u32,
    /// 分块大小（写入时实际使用的值，便于校验与向前兼容）。
    pub chunk_size: u64,
    /// 已写范围的 CRC 清单，必须无空洞地覆盖 `[0, written)`。
    pub chunks: Vec<ChunkCrc>,
}

impl WriteCheckpoint {
    /// `.resume` 路径（与源文件同目录）。
    pub fn path_for(file: &str) -> PathBuf {
        PathBuf::from(format!("{}{}", file, RESUME_SUFFIX))
    }


    /// 读取并严格校验。
    ///
    /// 返回值语义（关系到"绝不静默续写"这条铁律）：
    /// - `Ok(None)`：没有可用的断点（文件不存在 / 指纹不匹配），应从头写；
    /// - `Ok(Some(cp))`：断点完全可信，可以续传；
    /// - `Err(msg)`：**断点存在但不可信**（损坏、GPT 已变），必须中止并提示用户处理，
    ///   绝不能悄悄当成"没有断点"从头覆盖——那会让用户以为在续传。
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
        // 指纹不匹配：这是"另一个任务"的断点，不是损坏，从头写即可。
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
        // 结构自检：CRc 清单必须无空洞覆盖 [0, written)。
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

    /// 解析 `.resume` 文本（损坏即视为无效）。
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

    /// CRC 清单是否无空洞、无重叠地覆盖 `[0, written)`。
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

    /// 写出（原子替换：先写 `.tmp` 再 rename）。
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

    /// 删除续传文件（成功后调用）。
    pub fn clear(file: &str) {
        let _ = std::fs::remove_file(Self::path_for(file));
    }

    #[cfg(test)]
    /// 是否还有效（供测试/诊断）。
    pub fn exists(file: &str) -> bool {
        Path::new(&format!("{}{}", file, RESUME_SUFFIX)).exists()
    }
}

/// 解析 `offset:len:0xCRC`。
fn parse_chunk(v: &str) -> Option<ChunkCrc> {
    let mut it = v.split(':');
    let offset = it.next()?.parse::<u64>().ok()?;
    let len = it.next()?.parse::<u64>().ok()?;
    let crc = u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
    Some(ChunkCrc { offset, len, crc })
}

/// 增量 CRC32（IEEE 802.3，与 GPT 校验和一致）。
///
/// 用查表法：64 MiB 数据在 bitwise 实现下要跑 5 亿次循环，太慢。
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

    /// 从已完成的 CRC 值继续累加（用于续传时接着算同一块）。
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

/// 一次性计算 CRC32。
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

/// 源文件"指纹"：大小 + mtime 秒。
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

/// 分块 CRC 追踪器：按 `chunk_size` 切块，随写入增量累积。
///
/// 续传时用 [`CrcTracker::resume`] 从已有清单接着算，避免重读已写数据。
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

    /// 从已有清单恢复（最后一块若是"未填满的部分块"，会转为当前块继续累加）。
    pub fn resume(chunk_size: u64, mut chunks: Vec<ChunkCrc>) -> Self {
        let chunk_size = chunk_size.max(1);
        let (cur_off, cur_len, hasher) = match chunks.last().copied() {
            Some(last) if last.len < chunk_size => {
                // 部分块要从清单里摘掉，否则 snapshot 会重复计入同一块。
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

    /// 喂入一段已写入设备的数据（含补零）。
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

    /// 已累计的字节数（含当前未填满的块）。
    pub fn fed(&self) -> u64 {
        self.cur_off + self.cur_len
    }

    /// 从源文件把 `[fed(), to)` 追加进清单。
    ///
    /// 刻意在"设备确认写入成功后"才调用：预读阶段的字节可能因中途掉线并没有
    /// 真正落盘，提前计入会让清单比实际写入更长，续传时校验必然失败。
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
                // 源文件提前结束：与写入时一致，按零补齐。
                buf[n..want].fill(0);
            }
            self.feed(&buf[..want]);
            left -= want as u64;
        }
        Ok(())
    }

    /// 当前快照（含未填满的部分块）。
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
    /// 已完成块数（诊断用）。
    pub fn completed_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// 写入时使用的块大小。
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
        // 上次写到 1500（部分块）
        let mut t1 = CrcTracker::new(1024);
        t1.feed(&data[..1500]);
        let saved = t1.snapshot();
        // 续写：从 1500 开始，接着同一块继续算
        let mut t2 = CrcTracker::resume(1024, saved.clone());
        t2.feed(&data[1500..]);
        let merged = t2.snapshot();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].crc, crc32(&data[..1024]));
        assert_eq!(merged[1].crc, crc32(&data[1024..]));
    }

    #[test]
    fn crc_table_matches_bitwise_reference() {
        // 与 gpt.rs 的 bitwise 实现交叉验证，确保查表没写错。
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
