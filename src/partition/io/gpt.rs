//! GPT reading, caching, and partition address resolution

use crate::da::xflash::DAXFlash;
use crate::partition::GptInfo;
use crate::partition::io::log_gpt_crc_report;
use log::{debug, info, trace};

/// Get the GPT cache file path (under the tmp/ directory)
fn gpt_cache_file() -> std::path::PathBuf {
    crate::system::paths::get_tmp_path("gpt.bin")
}

/// Get the full GPT data file path (under the tmp/ directory)
fn gpt_full_file() -> std::path::PathBuf {
    crate::system::paths::get_tmp_path("gpt_full.bin")
}

impl<'a> DAXFlash<'a> {
    pub(crate) fn save_gpt_cache_file(path: &str, data: &[u8]) -> Result<(), String> {
        GptInfo::parse(data).map_err(|e| format!("GPT 缓存数据无效: {}", e))?;
        std::fs::write(path, data).map_err(|e| format!("写 GPT 缓存失败 '{}': {}", path, e))?;
        crate::connection::session::save_gpt_cache_path(path);
        Ok(())
    }

    pub(crate) fn load_gpt_cache_from_file(path: &str) -> Result<Vec<u8>, String> {
        let data =
            std::fs::read(path).map_err(|e| format!("读取 GPT 缓存失败 '{}': {}", path, e))?;
        GptInfo::parse(&data).map_err(|e| format!("GPT 缓存无效 '{}': {}", path, e))?;
        Ok(data)
    }

    pub(crate) fn try_load_cached_gpt(&mut self) -> bool {
        let Some(path) = crate::connection::session::get_gpt_cache_path() else {
            return false;
        };

        match Self::load_gpt_cache_from_file(&path) {
            Ok(data) => {
                debug!("复用 GPT 缓存: {} ({} 字节)", path, data.len());
                self.last_gpt_data = Some(data);
                true
            }
            Err(e) => {
                debug!("GPT 缓存不可用，将重新读取: {}", e);
                false
            }
        }
    }

    /// Read the GPT partition table (USB version)
    pub fn read_gpt(&mut self) -> Result<(), String> {
        info!("读取 GPT 分区表...");

        // Step 1: read 32KB first, covering most GPT headers + partition tables
        let initial_len: u64 = 32768;
        let mut gpt_data = self.readflash_data(0, initial_len)?;
        if gpt_data.len() < 604 {
            return Err("GPT 头数据不足（需要至少 604 字节）".to_string());
        }

        // Reuse GptInfo::parse to parse the GPT header (auto-locates the base by searching the EFI PART signature)
        {
            let gpt_info = GptInfo::parse(&gpt_data)?;
            log_gpt_crc_report(&gpt_info);
            let num_entries = gpt_info.num_part_entries as u64;
            let entry_size = gpt_info.part_entry_size as u64;
            let needed_len = 512 + num_entries * entry_size;

            debug!(
                "  GPT 分区数: {}, 分区项大小: {} 字节, 需要: {} 字节",
                num_entries, entry_size, needed_len
            );

            // Step 2: if 32KB is not enough, extend to read the full size
            if needed_len > initial_len {
                gpt_data = self.readflash_data(0, needed_len)?;
            }
        }
        debug!("  读取 GPT 数据: {} 字节", gpt_data.len());

        // Keep the raw data for debug mode
        self.last_gpt_data = Some(gpt_data.clone());

        // Write the full raw data to the tmp/ directory
        let gpt_full_path = gpt_full_file();
        std::fs::write(&gpt_full_path, &gpt_data)
            .map_err(|e| format!("写 gpt_full.bin 失败: {}", e))?;
        debug!(
            "已写入 {}, {} 字节",
            gpt_full_path.display(),
            gpt_data.len()
        );

        // Back up gpt.bin to the tmp/ directory (matching mtkclient behavior)
        let gpt_cache_path = gpt_cache_file();
        Self::save_gpt_cache_file(&gpt_cache_path.to_string_lossy(), &gpt_data)?;
        debug!(
            "已写入 {}, {} 字节",
            gpt_cache_path.display(),
            gpt_data.len()
        );

        // Parse-only validation. Full partition-table output happens only in the printgpt command;
        // reading GPT internally from read commands must not spam the console.
        GptInfo::parse(&gpt_data)?;
        Ok(())
    }

    /// Ensure the super dynamic partition metadata is loaded (single entry point, avoids duplicated code)
    ///
    /// Loading strategy:
    /// 1. If already cached, return directly
    /// 2. Try parsing from the first 1MB of the super partition (AOSP standard location)
    /// 3. Try parsing from the last 1MB of the super partition (backup geometry)
    /// 4. Try parsing from the first 1MB of the metadata partition (MTK Virtual A/B devices)
    pub(crate) fn ensure_super_metadata(&mut self) -> Result<(), String> {
        if self.super_metadata.is_some() {
            return Ok(());
        }

        // Get super partition info
        let (super_addr, super_size) = self
            .find_partition_addr("super")
            .map_err(|e| format!("查找 super 分区失败: {}", e))?;
        let meta_read_size = std::cmp::min(super_size, 1024 * 1024) as u64;

        // Strategy 1: first 1MB of the super partition (primary)
        match self.readflash_data(super_addr, meta_read_size) {
            Ok(primary_data) => {
                if let Ok(meta) = crate::partition::lp::SuperMetadata::parse(&primary_data) {
                    debug!(
                        "Super 动态分区解析成功（primary）: {} 个 logical partition",
                        meta.partitions.len()
                    );
                    self.super_metadata = Some(meta);
                    return Ok(());
                }
            }
            Err(e) => debug!("读取 super 分区失败: {}", e),
        }

        // Strategy 2: last 1MB of the super partition (backup)
        let backup_addr = super_addr + super_size.saturating_sub(meta_read_size);
        match self.readflash_data(backup_addr, meta_read_size) {
            Ok(backup_data) => {
                if let Ok(meta) = crate::partition::lp::SuperMetadata::parse(&backup_data) {
                    debug!(
                        "Super 动态分区解析成功（backup）: {} 个 logical partition",
                        meta.partitions.len()
                    );
                    self.super_metadata = Some(meta);
                    return Ok(());
                }
            }
            Err(e) => debug!("读取 super backup 失败: {}", e),
        }

        // Strategy 3: first 1MB of the metadata partition (MTK Virtual A/B)
        if let Ok((metadata_addr, metadata_size)) = self.find_partition_addr("metadata") {
            let md_read_size = std::cmp::min(metadata_size, 1024 * 1024) as u64;
            match self.readflash_data(metadata_addr, md_read_size) {
                Ok(metadata_data) => {
                    let non_zero = metadata_data.iter().filter(|&&b| b != 0).count();
                    debug!(
                        "metadata 数据中非零字节: {} / {}",
                        non_zero,
                        metadata_data.len()
                    );
                    if let Ok(meta) = crate::partition::lp::SuperMetadata::parse(&metadata_data) {
                        debug!(
                            "Super 动态分区解析成功（metadata 分区）: {} 个 logical partition",
                            meta.partitions.len()
                        );
                        self.super_metadata = Some(meta);
                        return Ok(());
                    }
                }
                Err(e) => debug!("读取 metadata 分区失败: {}", e),
            }
        }

        Err(
            "无法解析 super 动态分区元数据（super primary/backup 和 metadata 分区均失败）"
                .to_string(),
        )
    }

    /// Look up a partition's physical address and size
    /// eMMC hardware partitions (boot1/boot2/rpmb) are checked first, then GPT, then super dynamic partitions
    pub(crate) fn find_partition_addr(&mut self, partition: &str) -> Result<(u64, u64), String> {
        // 1. Try resolving special partitions first (boot1/boot2/rpmb)
        if let Some((_pt, addr, size)) = self.resolve_special_partition(partition) {
            return Ok((addr, size));
        }

        // 2. Load GPT and look it up
        if self.last_gpt_data.is_none() && !self.try_load_cached_gpt() {
            self.read_gpt()?;
        }
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;

        if let Some(entry) = gpt_info.find_partition(partition) {
            return Ok((entry.start_addr, entry.size));
        }

        // 2.5 A/B slot auto-resolution: if the bare name misses but name_a / name_b exists, append the suffix and retry
        if let Some(slotted) = crate::slot::resolve_slot_partition(self, partition) {
            let gpt_data = self
                .last_gpt_data
                .as_ref()
                .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;
            let gpt_info = GptInfo::parse(gpt_data)?;
            if let Some(entry) = gpt_info.find_partition(&slotted) {
                debug!("A/B 槽位解析: {} -> {}", partition, slotted);
                return Ok((entry.start_addr, entry.size));
            }
        }

        // 3. Not found in GPT: try resolving a logical partition from the super dynamic partitions
        if partition != "super" && partition != "metadata" {
            if self.super_metadata.is_none() {
                trace!("GPT 中未找到 {}，尝试从 super 动态分区解析...", partition);
                let _ = self.ensure_super_metadata();
            }

            if let Some(ref meta) = self.super_metadata {
                if let Some((actual_name, offset, size)) = meta.find_partition_smart(partition) {
                    let (super_addr, _) = self.find_partition_addr("super")?;
                    debug!(
                        "从 super 动态分区中找到 {}[{}]: 偏移 0x{:08X}, 大小 0x{:08X} ({}字节)",
                        partition, actual_name, offset, size, size
                    );
                    return Ok((super_addr + offset, size));
                }
            }
        }

        Err(format!("未找到分区: {}", partition))
    }

    /// Resolve special partition names (boot1/boot2/rpmb, etc.), returning (parttype, addr, size)
    /// parttype: 1=boot1, 2=boot2, 3=rpmb, 8=user
    pub(crate) fn resolve_special_partition(&mut self, name: &str) -> Option<(u32, u64, u64)> {
        let lower = name.to_lowercase();

        // 1. Prefer the exact size from get_emmc_info
        if let Ok(emmc) = self.get_emmc_info() {
            match lower.as_str() {
                "boot1" | "emmc_boot1" if emmc.boot1_size > 0 => {
                    return Some((1, 0, emmc.boot1_size));
                }
                "boot2" | "emmc_boot2" if emmc.boot2_size > 0 => {
                    return Some((2, 0, emmc.boot2_size));
                }
                "rpmb" | "emmc_rpmb" if emmc.rpmb_size > 0 => {
                    return Some((3, 0, emmc.rpmb_size));
                }
                _ => {}
            }
        }

        // 2. Fallback: fixed fallback when get_emmc_info fails (eMMC boot1/boot2 are standalone fixed 4MB hardware partitions)
        if lower == "boot1" || lower == "boot2" || lower == "emmc_boot1" || lower == "emmc_boot2" {
            if let Some(ref gpt_data) = self.last_gpt_data {
                if let Ok(gpt) = GptInfo::parse(gpt_data) {
                    if let Some(first) = gpt.partitions().first() {
                        // eMMC boot1/boot2 are standalone fixed hardware partitions whose size does not vary with
                        // the start address of user partitions, so they cannot be derived via first.start_addr / 2.
                        // When get_emmc_info fails, uniformly fall back to 4MB.
                        let size = 0x400000;
                        trace!(
                            "{} fallback: GPT 第一个分区起始=0x{:X}, 使用固定 boot 大小=0x{:X}",
                            name, first.start_addr, size
                        );
                        return Some((if lower.starts_with("boot1") { 1 } else { 2 }, 0, size));
                    }
                }
            }
            // Default 4MB when there is no GPT data
            trace!("{} fallback: 无 GPT 数据，默认大小=4MB", name);
            return Some((if lower.starts_with("boot1") { 1 } else { 2 }, 0, 0x400000));
        }

        // rpmb fallback defaults to 4MB
        if lower == "rpmb" || lower == "emmc_rpmb" {
            trace!("rpmb fallback: 默认大小=4MB");
            return Some((3, 0, 0x400000));
        }

        None
    }
}
