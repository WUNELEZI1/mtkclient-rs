//! GPT 读取、缓存与分区地址解析

use log::{debug, info, trace};
use crate::da::xflash::DAXFlash;
use crate::partition::GptInfo;
use crate::partition::io::log_gpt_crc_report;

/// 获取 GPT 缓存文件路径（位于 tmp/ 目录）
fn gpt_cache_file() -> std::path::PathBuf {
    crate::system::paths::get_tmp_path("gpt.bin")
}

/// 获取完整 GPT 数据文件路径（位于 tmp/ 目录）
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

    /// 读取 GPT 分区表（USB 版本）
    pub fn read_gpt(&mut self) -> Result<(), String> {
        info!("读取 GPT 分区表...");

        // 第一步：先读 32KB，覆盖大多数 GPT 头 + 分区表
        let initial_len: u64 = 32768;
        let mut gpt_data = self.readflash_data(0, initial_len)?;
        if gpt_data.len() < 604 {
            return Err("GPT 头数据不足（需要至少 604 字节）".to_string());
        }

        // 复用 GptInfo::parse 解析 GPT 头（自动搜索 EFI PART 签名定位基址）
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

            // 第二步：如果 32KB 不够，再扩展读取完整大小
            if needed_len > initial_len {
                gpt_data = self.readflash_data(0, needed_len)?;
            }
        }
        debug!("  读取 GPT 数据: {} 字节", gpt_data.len());

        // 保存原始数据供调试模式使用
        self.last_gpt_data = Some(gpt_data.clone());

        // 写入完整原始数据到 tmp/ 目录
        let gpt_full_path = gpt_full_file();
        std::fs::write(&gpt_full_path, &gpt_data)
            .map_err(|e| format!("写 gpt_full.bin 失败: {}", e))?;
        debug!(
            "已写入 {}, {} 字节",
            gpt_full_path.display(),
            gpt_data.len()
        );

        // 备份 gpt.bin 到 tmp/ 目录（对齐 mtkclient 行为）
        let gpt_cache_path = gpt_cache_file();
        Self::save_gpt_cache_file(&gpt_cache_path.to_string_lossy(), &gpt_data)?;
        debug!(
            "已写入 {}, {} 字节",
            gpt_cache_path.display(),
            gpt_data.len()
        );

        // 只做解析校验。完整分区表输出只在 printgpt 命令中执行，
        // 读分区命令内部读取 GPT 时不应刷屏。
        GptInfo::parse(&gpt_data)?;
        Ok(())
    }

    /// 确保 super 动态分区元数据已加载（统一入口，避免多处重复代码）
    ///
    /// 加载策略：
    /// 1. 如果已缓存，直接返回
    /// 2. 尝试从 super 分区前 1MB 解析（AOSP 标准位置）
    /// 3. 尝试从 super 分区末尾 1MB 解析（backup geometry）
    /// 4. 尝试从 metadata 分区前 1MB 解析（MTK Virtual A/B 设备）
    pub(crate) fn ensure_super_metadata(&mut self) -> Result<(), String> {
        if self.super_metadata.is_some() {
            return Ok(());
        }

        // 获取 super 分区信息
        let (super_addr, super_size) = self
            .find_partition_addr("super")
            .map_err(|e| format!("查找 super 分区失败: {}", e))?;
        let meta_read_size = std::cmp::min(super_size, 1024 * 1024) as u64;

        // 策略 1：super 分区前 1MB（primary）
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

        // 策略 2：super 分区末尾 1MB（backup）
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

        // 策略 3：metadata 分区前 1MB（MTK Virtual A/B）
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

    /// 查找分区的物理地址和大小
    /// 优先处理 eMMC 硬件分区（boot1/boot2/rpmb），然后查 GPT，最后查 super 动态分区
    pub(crate) fn find_partition_addr(&mut self, partition: &str) -> Result<(u64, u64), String> {
        // 1. 先尝试解析特殊分区（boot1/boot2/rpmb）
        if let Some((_pt, addr, size)) = self.resolve_special_partition(partition) {
            return Ok((addr, size));
        }

        // 2. 加载 GPT 并查找
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

        // 3. GPT 中未找到，尝试从 super 动态分区解析 logical partition
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

    /// 解析特殊分区名（boot1/boot2/rpmb 等），返回 (parttype, addr, size)
    /// parttype: 1=boot1, 2=boot2, 3=rpmb, 8=user
    pub(crate) fn resolve_special_partition(&mut self, name: &str) -> Option<(u32, u64, u64)> {
        let lower = name.to_lowercase();

        // 1. 优先通过 get_emmc_info 获取精确大小
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

        // 2. Fallback：get_emmc_info 失败时的固定回退（eMMC boot1/boot2 为独立固定 4MB 硬件分区）
        if lower == "boot1" || lower == "boot2" || lower == "emmc_boot1" || lower == "emmc_boot2" {
            if let Some(ref gpt_data) = self.last_gpt_data {
                if let Ok(gpt) = GptInfo::parse(gpt_data) {
                    if let Some(first) = gpt.partitions().first() {
                        // eMMC boot1/boot2 是独立的固定硬件分区，大小不随用户分区起始地址变化，
                        // 不能用 first.start_addr / 2 推算。get_emmc_info 失败时统一回退为 4MB。
                        let size = 0x400000;
                        trace!(
                            "{} fallback: GPT 第一个分区起始=0x{:X}, 使用固定 boot 大小=0x{:X}",
                            name, first.start_addr, size
                        );
                        return Some((if lower.starts_with("boot1") { 1 } else { 2 }, 0, size));
                    }
                }
            }
            // 无 GPT 数据时默认 4MB
            trace!("{} fallback: 无 GPT 数据，默认大小=4MB", name);
            return Some((if lower.starts_with("boot1") { 1 } else { 2 }, 0, 0x400000));
        }

        // rpmb fallback 默认 4MB
        if lower == "rpmb" || lower == "emmc_rpmb" {
            trace!("rpmb fallback: 默认大小=4MB");
            return Some((3, 0, 0x400000));
        }

        None
    }
}
