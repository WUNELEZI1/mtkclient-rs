use super::{ChunkCrc, Crc32};

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
