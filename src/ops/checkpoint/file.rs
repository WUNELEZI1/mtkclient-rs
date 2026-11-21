use std::io::Write;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use super::{CRC_CHUNK, RESUME_SUFFIX, VERSION, file_signature};

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
