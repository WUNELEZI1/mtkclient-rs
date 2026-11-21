//! Partition read/write/erase operations over DAXFlash
//!
//! - `read_gpt`   — read and save the GPT
//! - `find_partition_addr` — look up a partition address by name
//! - `read_partition` — read a partition into a file
//! - `write_partition` — write a file into a partition
//! - `erase_partition` — erase a partition
//! - `write_flash_data` / `cmd_write_data` / `get_packet_length` — low-level write primitives

pub(crate) mod crc;
pub(crate) mod gpt;
pub(crate) mod read;
pub(crate) mod resume;
pub(crate) mod write;

pub(crate) use crc::log_gpt_crc_report;
pub(crate) use resume::compute_read_resume_offset;

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

        // Missing → read from the start
        assert_eq!(compute_read_resume_offset(p, 1000), 0);

        // Complete → return size (caller skips accordingly, no re-read)
        std::fs::write(p, vec![0u8; 1000]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 1000), 1000);

        // Oversized (extreme case) → still return size
        std::fs::write(p, vec![0u8; 2000]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 1000), 1000);

        // Partial and 512-aligned → return the aligned offset
        std::fs::write(p, vec![0u8; 1024]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 2000), 1024);

        // Partial and unaligned → truncate to the 512 alignment point and return (critical rl resume path)
        std::fs::write(p, vec![0u8; 1500]).unwrap();
        assert_eq!(compute_read_resume_offset(p, 2000), 1024);
        assert_eq!(std::fs::metadata(p).unwrap().len(), 1024);

        let _ = std::fs::remove_file(p);
    }
}
