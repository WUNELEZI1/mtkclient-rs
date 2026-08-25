//! DAXFlash 上的分区读写擦操作
//!
//! - `read_gpt`   — 读取并保存 GPT
//! - `find_partition_addr` — 按名称查找分区地址
//! - `read_partition` — 读分区到文件
//! - `write_partition` — 写文件到分区
//! - `erase_partition` — 擦除分区
//! - `write_flash_data` / `cmd_write_data` / `get_packet_length` — 底层写入原语

pub(crate) mod resume;
pub(crate) mod crc;
pub(crate) mod gpt;
pub(crate) mod read;
pub(crate) mod write;

pub(crate) use resume::compute_read_resume_offset;
pub(crate) use crc::log_gpt_crc_report;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::DAXFlash;

    #[test]
    fn invalid_gpt_cache_is_rejected() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("invalid_gpt_cache_{}.bin", std::process::id()));
        std::fs::write(&path, b"not a gpt").unwrap();

        let err = DAXFlash::load_gpt_cache_from_file(path.to_str().unwrap()).unwrap_err();
        let _ = std::fs::remove_file(&path);

        assert!(err.contains("GPT 缓存无效"));
    }

    #[test]
    fn save_gpt_cache_rejects_invalid_data() {
        let path =
            std::env::temp_dir().join(format!("invalid_save_gpt_cache_{}.bin", std::process::id()));
        let err = DAXFlash::save_gpt_cache_file(path.to_str().unwrap(), b"not a gpt").unwrap_err();

        assert!(err.contains("GPT 缓存数据无效"));
        assert!(!path.exists());
    }

    #[test]
    fn compute_read_resume_offset_handles_states() {
        let path = std::env::temp_dir().join(format!("resume_offset_{}.bin", std::process::id()));
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);

        // 不存在 → 从头读取
        assert_eq!(compute_read_resume_offset(p, 1000), 0);

        // 已完整 → 返回 size（调用方据此跳过，不再重读）
        std::fs::write(p, vec![0u8; 1000]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 1000), 1000);

        // 偏大（极端情况）→ 仍返回 size
        std::fs::write(p, vec![0u8; 2000]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 1000), 1000);

        // 部分且 512 对齐 → 返回对齐偏移
        std::fs::write(p, vec![0u8; 1024]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 2000), 1024);

        // 部分且未对齐 → 截断到 512 对齐点并返回（rl 续传关键路径）
        std::fs::write(p, vec![0u8; 1500]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 2000), 1024);
        assert_eq!(std::fs::metadata(p).unwrap().len(), 1024);

        let _ = std::fs::remove_file(p);
    }
}
