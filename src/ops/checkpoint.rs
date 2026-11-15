//! 写入断点续传（checkpoint）。
//!
//! 设计要点：
//! - **与后端无关**：真机（`DAXFlash`）与仿真（`da::engine`）共用同一份 checkpoint；
//! - **只认强证据**：分区名、目标地址、总长度、源文件大小与 mtime 全部匹配才续传，
//!   任何一项变化都从头开始（宁可重写，不要写错位）；
//! - **写盘时机**：每 8 MiB 落一次、出错时立刻落一次，进程被杀最多丢 8 MiB；
//! - **完成后删除**：成功后删除 `.resume`，避免下次误续传。

use std::io::Write;
use std::path::{Path, PathBuf};

/// checkpoint 版本（格式变更时递增，旧文件自动失效）。
const VERSION: u32 = 1;

/// 续传状态文件后缀。
pub const RESUME_SUFFIX: &str = ".resume";

/// 落盘间隔。
pub const PERSIST_INTERVAL: u64 = 8 * 1024 * 1024;

/// 一次写入的续传状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteCheckpoint {
    pub part: String,
    pub addr: u64,
    pub total: u64,
    pub written: u64,
    pub file_size: u64,
    pub file_mtime: u64,
}

impl WriteCheckpoint {
    /// `.resume` 路径（与源文件同目录）。
    pub fn path_for(file: &str) -> PathBuf {
        PathBuf::from(format!("{}{}", file, RESUME_SUFFIX))
    }

    /// 读取并校验：只有与本文件/本分区完全匹配时才返回。
    pub fn load(file: &str, part: &str, addr: u64, total: u64) -> Option<Self> {
        let path = Self::path_for(file);
        let text = std::fs::read_to_string(&path).ok()?;
        let cp = Self::parse(&text)?;
        let (size, mtime) = file_signature(file)?;
        if cp.part == part
            && cp.addr == addr
            && cp.total == total
            && cp.file_size == size
            && cp.file_mtime == mtime
            && cp.written > 0
            && cp.written < total
        {
            Some(cp)
        } else {
            None
        }
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
        })
    }

    /// 写出（原子替换：先写 `.tmp` 再 rename）。
    pub fn save(&self, file: &str) -> Result<(), String> {
        let path = Self::path_for(file);
        let tmp = path.with_extension("resume.tmp");
        let text = format!(
            "version={}\npart={}\naddr=0x{:X}\ntotal={}\nwritten={}\nfile_size={}\nfile_mtime={}\n",
            VERSION,
            self.part,
            self.addr,
            self.total,
            self.written,
            self.file_size,
            self.file_mtime
        );
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

    /// 是否还有效（供测试/诊断）。
    pub fn exists(file: &str) -> bool {
        Path::new(&format!("{}{}", file, RESUME_SUFFIX)).exists()
    }
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

    #[test]
    fn roundtrip_and_match() {
        let dir = tmpdir();
        let file = dir.join("boot.img");
        std::fs::write(&file, vec![7u8; 2048]).unwrap();
        let f = file.to_str().unwrap();
        let (size, mtime) = file_signature(f).unwrap();
        let cp = WriteCheckpoint {
            part: "boot_a".into(),
            addr: 0x100000,
            total: 2048,
            written: 1024,
            file_size: size,
            file_mtime: mtime,
        };
        cp.save(f).unwrap();
        let got = WriteCheckpoint::load(f, "boot_a", 0x100000, 2048).unwrap();
        assert_eq!(got, cp);
        // 分区名不同 → 不续传
        assert!(WriteCheckpoint::load(f, "boot_b", 0x100000, 2048).is_none());
        // 源文件变了 → 不续传
        std::fs::write(&file, vec![8u8; 4096]).unwrap();
        assert!(WriteCheckpoint::load(f, "boot_a", 0x100000, 2048).is_none());
        WriteCheckpoint::clear(f);
        assert!(!WriteCheckpoint::exists(f));
    }

    #[test]
    fn corrupt_checkpoint_is_ignored() {
        assert!(WriteCheckpoint::parse("garbage").is_none());
        assert!(WriteCheckpoint::parse("version=1\npart=x\n").is_none());
    }
}
