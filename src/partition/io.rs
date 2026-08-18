//! DAXFlash 上的分区读写擦操作
//!
//! - `read_gpt`   — 读取并保存 GPT
//! - `find_partition_addr` — 按名称查找分区地址
//! - `read_partition` — 读分区到文件
//! - `write_partition` — 写文件到分区
//! - `erase_partition` — 擦除分区
//! - `write_flash_data` / `cmd_write_data` / `get_packet_length` — 底层写入原语

use log::{debug, info, trace, warn};
use std::io::Read;
use std::sync::atomic::Ordering;

use crate::da::xflash::{CMD_FORMAT, CMD_MAGIC, DAXFlash, pack3};
use crate::usb::log::QUIET_USB_READ;

use super::gpt::GptInfo;
use super::write::{check_write_resume, remove_write_resume_file};

/// 在文件末尾填充零字节的 Reader 包装器，用于 512 字节对齐。
struct PaddedReader<R: Read> {
    inner: R,
    remaining: u64, // 剩余需要读取的总字节数（包含填充）
}

impl<R: Read> PaddedReader<R> {
    fn new(inner: R, file_size: u64, pad_to: u64) -> Self {
        let padded = if file_size % pad_to != 0 {
            file_size + (pad_to - file_size % pad_to)
        } else {
            file_size
        };
        Self {
            inner,
            remaining: padded,
        }
    }
}

impl<R: Read> Read for PaddedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let to_read = std::cmp::min(buf.len() as u64, self.remaining) as usize;
        let n = self.inner.read(&mut buf[..to_read])?;
        if n == 0 {
            // 文件已读完，填充零字节
            let fill = std::cmp::min(buf.len(), self.remaining as usize);
            for b in &mut buf[..fill] {
                *b = 0;
            }
            self.remaining -= fill as u64;
            return Ok(fill);
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// 获取 GPT 缓存文件路径（位于 tmp/ 目录）
fn gpt_cache_file() -> std::path::PathBuf {
    crate::system::paths::获取tmp路径("gpt.bin")
}

/// 获取完整 GPT 数据文件路径（位于 tmp/ 目录）
fn gpt_full_file() -> std::path::PathBuf {
    crate::system::paths::获取tmp路径("gpt_full.bin")
}

/// 计算分区读取的断点续传起始偏移（供单分区 `读取分区` 与 `rl` 批量读取共用）。
///
/// 规则：
/// - 输出文件不存在或为空 → 返回 `0`（从头读取）。
/// - 文件已完整（现有大小 >= 目标大小）→ 返回 `目标大小`，调用方据此判定“已完成”并跳过。
/// - 文件部分存在 → **截断到 512 字节对齐边界**（避免续读错位），并返回该对齐偏移作为续传起点。
///
/// ⚠️ 副作用警告：本函数并非纯查询。对“部分存在”的文件，它会**直接修改磁盘**
/// （`set_len` 截断到 512 对齐点），即使调用方只是想“判断是否需要续传”。
/// 调用前请确认已接受该文件可能被截断；续读逻辑依赖此对齐保证数据不重叠、不错位。
pub(crate) fn compute_read_resume_offset(output: &str, size: u64) -> u64 {
    let path = std::path::Path::new(output);
    let existing = if path.exists() {
        std::fs::metadata(output).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if existing == 0 {
        return 0;
    }
    if existing >= size {
        return size; // 调用方据此跳过
    }
    // 部分存在：截断到 512 字节对齐边界，从对齐点续读
    let aligned = (existing / 512) * 512;
    if aligned != existing {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(output) {
            let _ = file.set_len(aligned);
        }
    }
    aligned
}

impl<'a> DAXFlash<'a> {
    pub(crate) fn save_gpt_cache_file(path: &str, data: &[u8]) -> Result<(), String> {
        GptInfo::parse(data).map_err(|e| format!("GPT 缓存数据无效: {}", e))?;
        std::fs::write(path, data).map_err(|e| format!("写 GPT 缓存失败 '{}': {}", path, e))?;
        crate::connection::session::save_gpt_cache_path(path);
        Ok(())
    }

    fn load_gpt_cache_from_file(path: &str) -> Result<Vec<u8>, String> {
        let data =
            std::fs::read(path).map_err(|e| format!("读取 GPT 缓存失败 '{}': {}", path, e))?;
        GptInfo::parse(&data).map_err(|e| format!("GPT 缓存无效 '{}': {}", path, e))?;
        Ok(data)
    }

    fn try_load_cached_gpt(&mut self) -> bool {
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
    fn resolve_special_partition(&mut self, name: &str) -> Option<(u32, u64, u64)> {
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

        // 2. Fallback：从 GPT 第一个分区的起始地址推断 boot1/boot2 大小
        // 若 GPT 第一个分区起始地址 > 0，则前面空间均分给 boot1 和 boot2
        if lower == "boot1" || lower == "boot2" || lower == "emmc_boot1" || lower == "emmc_boot2" {
            if let Some(ref gpt_data) = self.last_gpt_data {
                if let Ok(gpt) = GptInfo::parse(gpt_data) {
                    if let Some(first) = gpt.partitions().first() {
                        let inferred = first.start_addr / 2;
                        // 合理性检查：推断值至少 1MB 才使用，否则默认 4MB
                        let size = if inferred >= 0x10_0000 {
                            inferred
                        } else {
                            0x400000
                        };
                        trace!(
                            "{} fallback: GPT 第一个分区起始=0x{:X}, 推断大小=0x{:X}",
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

    /// 读取分区数据到文件
    /// 支持断点续传：检测已有文件大小，从断点继续读取
    /// 流式写入：每个 USB 数据包收到后立即写入磁盘，内存占用极低
    /// 进度显示：使用自研进度条（src/progress.rs），每包更新
    pub fn 读取分区(&mut self, 分区名: &str, 输出文件: &str) -> Result<(), String> {
        use crate::progress::{ProgressBar, ProgressStyle};

        // 解析分区信息（特殊分区或 GPT 分区）
        let (parttype, addr, size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(分区名) {
                info!(
                    "读取特殊分区 {} (parttype={}), addr=0x{:X}, size={} 字节",
                    分区名, pt, addr, size
                );
                (pt, addr, size)
            } else {
                if self.last_gpt_data.is_none() && !self.try_load_cached_gpt() {
                    self.read_gpt()?;
                }
                // find_partition_addr 内部已处理 GPT 物理分区 → super 动态分区回退
                let (addr, size) = self.find_partition_addr(分区名)?;
                info!(
                    "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
                    分区名, addr, size
                );
                (8u32, addr, size)
            };

        // 解析输出路径：若为目录（已存在目录 / 以路径分隔符结尾 / 无扩展名且不存在），
        // 则在该目录内写入 <分区名>.img，对齐 rl 命令行为，避免误报“输出路径是目录”。
        let 输出路径 = std::path::Path::new(输出文件);
        let 实际输出: String = if 输出路径.is_dir()
            || 输出文件.ends_with('/')
            || 输出文件.ends_with('\\')
            || (!输出路径.exists() && 输出路径.extension().is_none())
        {
            format!("{}/{}.img", 输出文件.trim_end_matches(['/', '\\']), 分区名)
        } else {
            输出文件.to_string()
        };
        let 输出文件 = 实际输出.as_str();
        let 输出路径 = std::path::Path::new(输出文件);
        if let Some(parent) = 输出路径.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录失败 '{}': {}", parent.display(), e))?;
        }

        // 断点续传检查（复用统一辅助函数：完整则跳过、部分则截断到 512 对齐并返回偏移）
        let start_offset = compute_read_resume_offset(输出文件, size);
        if start_offset >= size {
            info!("  文件已存在且完整 ({} 字节)，跳过读取", start_offset);
            return Ok(());
        }
        if start_offset > 0 {
            info!(
                "  断点续传: 已有 {} 字节 ({}%)，从 0x{:X} 继续读取",
                start_offset,
                start_offset as f64 / size as f64 * 100.0,
                start_offset
            );
        }

        // 创建进度条（输出到 stderr，与日志统一流，避免视觉交织）
        let bar = if QUIET_USB_READ.load(Ordering::Relaxed) {
            ProgressBar::hidden()
        } else {
            ProgressBar::new(size)
        };
        bar.set_style(
            ProgressStyle::with_template(
                "  {spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {binary_bytes}/{binary_total_bytes} ({percent}%) \
             {msg} ETA {eta}",
            )
            .unwrap()
            .progress_chars("█▓░"),
        );
        bar.set_message(format!("读取: {}", 分区名));
        if start_offset > 0 {
            bar.set_position(start_offset);
        }

        // 滑动窗口速度计算（10 秒窗口，每 4MB 采样一次）
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        let 速度窗口大小: u64 = 10;
        let 速度采样间隔: u64 = 4 * 1024 * 1024; // 4MB
        let 速度窗口: Arc<Mutex<VecDeque<(std::time::Instant, u64)>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(64)));
        let 上次速度采样: Arc<Mutex<u64>> = Arc::new(Mutex::new(start_offset));

        // 流式读取：每个 USB 包写入文件后立即更新进度条
        // 注意：readflash_to_file 内部会在非活跃续传分支按 (addr + start_offset) 重新下发
        // READ_DATA，故此处必须传“分区基址”(addr) 而非已偏移地址，否则会出现双重偏移、
        // 读到分区之外的错误区域（数据损坏）。活跃续传分支只发 ACK，不使用 addr。
        let 分区名clone = 分区名.to_string();
        let total =
            self.readflash_to_file(addr, size, parttype, 输出文件, start_offset, {
                let bar = bar.clone();
                let 速度窗口 = 速度窗口.clone();
                let 上次速度采样 = 上次速度采样.clone();
                move |bytes_read| {
                    bar.set_position(bytes_read);

                    // 滑动窗口速度计算
                    let now = std::time::Instant::now();
                    let 上次 = *上次速度采样.lock().unwrap();
                    let delta = bytes_read.saturating_sub(上次);
                    if delta >= 速度采样间隔 || bytes_read == size {
                        {
                            let mut 窗口 = 速度窗口.lock().unwrap();
                            窗口.push_back((now, bytes_read));
                            *上次速度采样.lock().unwrap() = bytes_read;

                            // 移除过期的采样点（VecDeque::pop_front 为 O(1)）
                            let 截止 = now - std::time::Duration::from_secs(速度窗口大小);
                            while 窗口.len() > 2 && 窗口.front().unwrap().0 < 截止 {
                                窗口.pop_front();
                            }

                            // 计算窗口平均速度
                            if 窗口.len() >= 2 {
                                let 首次 = 窗口.front().unwrap();
                                let 末次 = 窗口.back().unwrap();
                                let 时间差 = 末次.0.duration_since(首次.0).as_secs_f64();
                                if 时间差 > 0.01 {
                                    let 字节差 = 末次.1.saturating_sub(首次.1);
                                    let 速度_mib = (字节差 as f64 / 1024.0 / 1024.0) / 时间差;
                                    bar.set_message(format!(
                                        "读取: {} {:.2} MB/s",
                                        分区名clone, 速度_mib
                                    ));
                                }
                            }
                        }
                    }
                }
            })?;

        bar.finish_with_message(format!("{} 读取完成 ({} 字节)", 分区名, total));
        info!("  已保存到: {}", 输出文件);
        Ok(())
    }

    /// 读取 super 分区内的动态分区（logical partition）
    ///
    /// 用法: 读取动态分区 "system" 到 "system.img"
    /// 内部通过 super 元数据解析逻辑分区的物理偏移。
    pub fn 读取动态分区(
        &mut self, 逻辑分区名: &str, 输出文件: &str
    ) -> Result<(), String> {
        // 1. 找到 super 物理分区地址
        let (super_addr, _super_size) = self.find_partition_addr("super")?;

        // 2. 确保 super 元数据已加载（统一入口：super → backup → metadata）
        if self.super_metadata.is_none() {
            self.ensure_super_metadata()
                .map_err(|e| format!("加载 super 元数据失败: {}", e))?;
        }

        // 3. 在 super 元数据中查找逻辑分区（自动处理 A/B 槽位）
        let meta = self.super_metadata.as_ref().ok_or("super 元数据不可用")?;
        let (actual_name, logical_offset, logical_size) = meta
            .find_partition_smart(逻辑分区名)
            .ok_or_else(|| format!("在 super 中未找到逻辑分区: {}", 逻辑分区名))?;

        let 物理地址 = super_addr + logical_offset;
        debug!(
            "读取动态分区 {}[{}]: super[0x{:08X}] + offset[0x{:08X}] = 物理地址 0x{:08X}, 大小 {} 字节",
            逻辑分区名, actual_name, super_addr, logical_offset, 物理地址, logical_size
        );

        // 4. 断点续传检查：对齐到 512 字节边界（复用统一辅助 compute_read_resume_offset，
        //    与 读取分区 / rl 共用同一“截断对齐 + 已完成跳过”逻辑，避免行为漂移；
        //    且该辅助在 existing >= size 时返回 size，可避免 start_offset 越界导致
        //    后续 readflash_to_file 中 size - start_offset 的 u64 下溢）。
        let start_offset = compute_read_resume_offset(输出文件, logical_size);
        if start_offset > 0 && start_offset < logical_size {
            info!(
                "  断点续传: 已有 {} 字节 ({}%)，还需读取 {} 字节",
                start_offset,
                start_offset as f64 / logical_size as f64 * 100.0,
                logical_size - start_offset
            );
        }
        if start_offset >= logical_size {
            info!("  文件已存在且完整 ({} 字节)，跳过读取", start_offset);
            return Ok(());
        }

        // 5. 创建进度条并流式读取
        use crate::progress::{ProgressBar, ProgressStyle};
        use std::sync::atomic::Ordering;
        let bar = if crate::usb::log::QUIET_USB_READ.load(Ordering::Relaxed) {
            ProgressBar::hidden()
        } else {
            ProgressBar::new(logical_size)
        };
        bar.set_style(
            ProgressStyle::with_template(
                "  {spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
             {binary_bytes}/{binary_total_bytes} ({percent}%) \
             {binary_bytes_per_sec} {msg} ETA {eta}",
            )
            .unwrap()
            .progress_chars("█▓░"),
        );
        bar.set_message(format!("读取: {}", actual_name));
        if start_offset > 0 {
            bar.set_position(start_offset);
        }

        let bar_clone = bar.clone();
        // 只在文件完全不存在或大小为 0 时清理 resume
        // （保留 resume 文件：如果设备仍在发送数据流中，active_resume 路径
        //   会通过 ACK 接续；如果设备已断开，active_resume ACK 失败后会
        //   自动 fallback 到非活跃路径发新 READ_DATA）
        if start_offset == 0 {
            let resume_path = crate::resume::read_resume_path(输出文件);
            let _ = std::fs::remove_file(&resume_path);
        }
        self.readflash_to_file(
            物理地址,
            logical_size,
            8,
            输出文件,
            start_offset,
            move |bytes_read| {
                bar_clone.set_position(bytes_read);
            },
        )?;
        bar.finish_with_message("完成");
        info!("  动态分区 {} 已保存到: {}", 逻辑分区名, 输出文件);
        Ok(())
    }

    /// 写入文件到分区（带校验）
    /// 分块流式校验：避免大分区全量读入内存导致 OOM。
    /// 支持断点续传：续传时只校验本次写入的部分。
    pub fn 写入分区带校验(
        &mut self, 分区名: &str, 输入文件: &str
    ) -> Result<(), String> {
        // 先写入（写入分区内部处理断点续传）
        self.写入分区(分区名, 输入文件)?;

        // 分块流式校验：只校验本次写入的部分
        info!("  开始分块校验写入数据...");
        use std::io::{BufReader, Read, Seek, SeekFrom};
        let file = std::fs::File::open(输入文件).map_err(|e| format!("读取原始文件失败: {}", e))?;
        let mut reader = BufReader::new(file);

        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())?;
        let gpt_info = GptInfo::parse(gpt_data)?;
        let entry = gpt_info
            .find_partition(分区名)
            .ok_or_else(|| format!("未找到分区: {}", 分区名))?;

        // 计算校验起始位置：写入可能续传，只校验本次写入的部分
        let 写入大小 = {
            let 文件元数据 =
                std::fs::metadata(输入文件).map_err(|e| format!("无法获取文件元数据: {}", e))?;
            let 文件大小 = 文件元数据.len();
            if 文件大小 % 512 != 0 {
                文件大小 + (512 - 文件大小 % 512)
            } else {
                文件大小
            }
        };
        // 检查是否是续传写入（resume 文件已删除，但可通过写入大小推算）
        // 实际上 resume 文件已在 写入分区 成功后被删除，
        // 所以这里始终从 0 开始校验是安全的做法（写入已完成）。
        let verify_start = 0u64;

        // Seek 到校验起始位置
        if verify_start > 0 {
            reader
                .seek(SeekFrom::Start(verify_start))
                .map_err(|e| format!("文件 seek 失败: {}", e))?;
        }

        const VERIFY_CHUNK: usize = 0x100000; // 1MB 校验块（减少读取次数）
        let mut offset = verify_start;
        let mut buf = vec![0u8; VERIFY_CHUNK];

        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| format!("读取原始文件失败: {}", e))?;
            if n == 0 {
                break;
            }
            // 不超过文件实际大小（避免校验 padding 的零字节）
            if offset >= 写入大小 {
                break;
            }
            let effective_n = std::cmp::min(n, (写入大小.saturating_sub(offset)) as usize);
            let chunk = &buf[..effective_n];
            let device_data = self.readflash_data(entry.start_addr + offset, effective_n as u64)?;
            if device_data != chunk {
                return Err(format!(
                    "校验失败 @ offset 0x{:X}: 写入数据与原始数据不匹配",
                    entry.start_addr + offset
                ));
            }
            offset += effective_n as u64;
        }

        info!("  校验通过 (共 {} 字节)", offset);
        Ok(())
    }

    /// 写入文件到分区（流式，避免大文件 OOM）
    /// 对齐 Python writeflash (xflash_lib.py:writeflash)
    /// 协议: cmd_write_data → 循环分包写入 [0x0(4B)][checksum(4B)][data] → CC_OPTIONAL_DOWNLOAD_ACT → status
    /// 增加校验：文件大小不能超过分区大小，超过时报错。
    /// 支持断点续传：取消后可通过 `.wresume` 文件从断点继续。
    pub fn 写入分区(&mut self, 分区名: &str, 输入文件: &str) -> Result<(), String> {
        info!("写入文件 {} 到分区 {}...", 输入文件, 分区名);

        // 打开文件（流式读取，避免全量载入内存）
        let file = std::fs::File::open(输入文件).map_err(|e| format!("无法打开文件: {}", e))?;
        let 文件元数据 = file
            .metadata()
            .map_err(|e| format!("无法获取文件元数据: {}", e))?;
        let 文件大小 = 文件元数据.len();

        // 先尝试解析特殊分区（boot1/boot2/rpmb）
        let (parttype, 地址, 分区大小) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(分区名) {
                info!("写入特殊分区 {} (parttype={})", 分区名, pt);
                (pt, addr, size)
            } else {
                // 如果无 GPT 缓存，先尝试从 gpt.bin 加载缓存，再从设备读取
                if self.last_gpt_data.is_none() {
                    if !self.try_load_cached_gpt() {
                        self.read_gpt()?;
                    }
                }
                // find_partition_addr 内部已处理 GPT 物理分区 → super 动态分区回退
                let (addr, size) = self.find_partition_addr(分区名)?;
                (8u32, addr, size) // parttype = USER
            };

        // 校验：文件大小不能超过分区大小
        if 文件大小 > 分区大小 {
            return Err(format!(
                "文件大小 ({}) 超过分区 {} 容量 ({} 字节)，写入被拒绝",
                文件大小, 分区名, 分区大小
            ));
        }
        if 文件大小 == 分区大小 {
            info!("  文件大小与分区容量完全匹配");
        } else {
            info!("  文件大小: {} 字节, 分区容量: {} 字节", 文件大小, 分区大小);
        }

        // 使用 PaddedReader 自动处理 512 字节对齐填充
        let 写入大小 = if 文件大小 % 512 != 0 {
            文件大小 + (512 - 文件大小 % 512)
        } else {
            文件大小
        };

        // 检查写入断点续传
        let start_offset = match check_write_resume(输入文件, 地址, 写入大小) {
            Some(offset) => {
                info!(
                    "  断点续传: 从 0x{:X} ({}) 继续，已写入 {}/{} 字节 ({:.1}%)",
                    offset,
                    offset,
                    offset,
                    写入大小,
                    offset as f64 / 写入大小 as f64 * 100.0
                );
                offset
            }
            None => 0,
        };

        // 打开文件并 seek 到断点位置
        use std::io::{Seek, SeekFrom};
        let file_for_seek =
            std::fs::File::open(输入文件).map_err(|e| format!("无法打开文件: {}", e))?;
        let mut file_seekable = file_for_seek;
        if start_offset > 0 {
            file_seekable
                .seek(SeekFrom::Start(start_offset))
                .map_err(|e| format!("文件 seek 失败: {}", e))?;
        }

        let remaining = 写入大小.saturating_sub(start_offset);
        let padded_reader =
            PaddedReader::new(file_seekable, 文件大小.saturating_sub(start_offset), 512);
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, padded_reader);

        let write_addr = 地址 + start_offset;

        // 使用 BufReader 流式读取 + write_flash_data_stream
        self.write_flash_data_stream(
            write_addr,
            &mut reader,
            remaining,
            1,
            parttype,
            start_offset,
            输入文件,
        )?;

        // 成功：删除续传状态文件
        remove_write_resume_file(输入文件);
        info!("  写入完成: {} 字节写入分区 {}", 文件大小, 分区名);
        Ok(())
    }

    /// 擦除分区
    /// 对齐 Python formatflash (xflash_lib.py:formatflash)
    /// 协议: FORMAT 命令 → send_param → 等待 STATUS_COMPLETE (0x40040005)
    pub fn 擦除分区(&mut self, 分区名: &str) -> Result<(), String> {
        info!("擦除分区 {}...", 分区名);

        // 先尝试解析特殊分区（boot1/boot2/rpmb）
        let (parttype, 地址, 大小) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(分区名) {
                info!("擦除特殊分区 {} (parttype={})", 分区名, pt);
                (pt, addr, size)
            } else {
                // 如果无 GPT 缓存，自动读取 GPT 数据
                if self.last_gpt_data.is_none() {
                    self.read_gpt()?;
                }
                // 找到分区地址和大小
                let (addr, size) = self.find_partition_addr(分区名)?;
                (8u32, addr, size) // parttype = USER
            };

        // 发送 FORMAT 命令
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_FORMAT.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("FORMAT 命令 status error: 0x{:08X}", st));
        }

        // 发送参数: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = EMMC
        param.extend_from_slice(&parttype.to_le_bytes()); // parttype
        param.extend_from_slice(&地址.to_le_bytes());
        param.extend_from_slice(&大小.to_le_bytes());
        // NandExtension 全零（对齐 Python xflash_flash_param.py）
        param.extend_from_slice(&[0u8; 32]);

        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // 等待 STATUS_COMPLETE (0x40040005) 或 STATUS_CONTINUE (0x40040004)
        // 注意：FORMAT 命令完成后，部分 DA 版本返回 0x00000000 (ACK) 而非 0x40040005
        const STATUS_COMPLETE: u32 = 0x40040005;
        const STATUS_CONTINUE: u32 = 0x40040004;

        let erase_start = std::time::Instant::now();
        let mut status = self.status()?;
        while status == STATUS_CONTINUE {
            // STATUS_CONTINUE 包含等待时间（毫秒）
            let wait_ms = self.status()?;
            // 进度提示：每 5 秒输出一次等待信息
            let elapsed = erase_start.elapsed().as_secs();
            if elapsed > 0 && elapsed % 5 == 0 {
                info!(
                    "  擦除中... 已等待 {}s (分区 {}, {:.2} MB)",
                    elapsed,
                    分区名,
                    大小 as f64 / 1024.0 / 1024.0
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
            let _ = self.ack();
            status = self.status()?;
        }

        // FORMAT 成功状态：STATUS_COMPLETE 或 0x00000000 (ACK)
        if status != STATUS_COMPLETE && status != 0x00000000 {
            return Err(format!("擦除失败: status=0x{:08X}", status));
        }

        // STATUS_COMPLETE 后必须发送 ACK 完成 FORMAT 事务，
        // 否则 DA 卡在等待 ACK 状态，下一条命令会超时
        if status == STATUS_COMPLETE {
            let _ = self.ack();
        }

        // 擦除完成后排空 USB 管道残留
        // 不用 recover_usb_pipes：clear_halt 会重置 data toggle 导致后续读取错位
        self.preloader.device.drain_pipes();

        info!("  擦除完成: 分区 {} (0x{:X} @ 0x{:X})", 分区名, 大小, 地址);
        Ok(())
    }

    /// 解锁 Bootloader
    /// 对齐 Python mtkclient: unlock 命令
    /// 流程: 读取 seccfg 分区 → 解析 V4/V3 结构 → 修改 lock_state → 重新签名 → 写回
    pub fn unlock_bootloader(&mut self) -> Result<(), String> {
        crate::security::seccfg::unlock_bootloader(self)
    }

    /// 锁定 Bootloader
    // 预留：unlock/lock 命令使用
    pub fn lock_bootloader(&mut self) -> Result<(), String> {
        crate::security::seccfg::lock_bootloader(self)
    }
}

fn log_gpt_crc_report(gpt_info: &GptInfo<'_>) {
    match gpt_info.crc_report() {
        Ok(report) => {
            if report.header_ok() {
                debug!(
                    "GPT Header CRC 校验成功: 0x{:08X}",
                    report.stored_header_crc32
                );
            } else {
                warn!(
                    "GPT Header CRC 校验失败: 原CRC=0x{:08X}, 计算CRC=0x{:08X}",
                    report.stored_header_crc32, report.calculated_header_crc32
                );
            }

            if report.partition_entries_ok() {
                debug!(
                    "GPT 分区条目 CRC 校验成功: 0x{:08X}",
                    report.stored_partition_entries_crc32
                );
            } else {
                warn!(
                    "GPT 分区条目 CRC 校验失败: 原CRC=0x{:08X}, 计算CRC=0x{:08X}",
                    report.stored_partition_entries_crc32,
                    report.calculated_partition_entries_crc32
                );
            }
        }
        Err(e) => warn!("GPT CRC 校验跳过: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
