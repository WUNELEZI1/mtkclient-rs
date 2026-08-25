//! 分区读取（单分区 / 动态分区）

use crate::da::xflash::DAXFlash;
use crate::partition::io::compute_read_resume_offset;
use crate::usb::log::QUIET_USB_READ;
use log::{debug, info};
use std::sync::atomic::Ordering;

impl<'a> DAXFlash<'a> {
    /// 读取分区数据到文件
    /// 支持断点续传：检测已有文件大小，从断点继续读取
    /// 流式写入：每个 USB 数据包收到后立即写入磁盘，内存占用极低
    /// 进度显示：使用自研进度条（src/progress.rs），每包更新
    pub fn read_partition(&mut self, part_name: &str, out_file: &str) -> Result<(), String> {
        use crate::progress::{ProgressBar, ProgressStyle};

        // 解析分区信息（特殊分区或 GPT 分区）
        let (parttype, addr, size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(part_name) {
                info!(
                    "读取特殊分区 {} (parttype={}), addr=0x{:X}, size={} 字节",
                    part_name, pt, addr, size
                );
                (pt, addr, size)
            } else {
                if self.last_gpt_data.is_none() && !self.try_load_cached_gpt() {
                    self.read_gpt()?;
                }
                // find_partition_addr 内部已处理 GPT 物理分区 → super 动态分区回退
                let (addr, size) = self.find_partition_addr(part_name)?;
                info!(
                    "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
                    part_name, addr, size
                );
                (8u32, addr, size)
            };

        // 解析输出路径：若为目录（已存在目录 / 以路径分隔符结尾 / 无扩展名且不存在），
        // 则在该目录内写入 <分区名>.img，对齐 rl 命令行为，避免误报“输出路径是目录”。
        let out_path = std::path::Path::new(out_file);
        let actual_output: String = if out_path.is_dir()
            || out_file.ends_with('/')
            || out_file.ends_with('\\')
            || (!out_path.exists() && out_path.extension().is_none())
        {
            format!(
                "{}/{}.img",
                out_file.trim_end_matches(['/', '\\']),
                part_name
            )
        } else {
            out_file.to_string()
        };
        let out_file = actual_output.as_str();
        let out_path = std::path::Path::new(out_file);
        if let Some(parent) = out_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录失败 '{}': {}", parent.display(), e))?;
        }

        // 断点续传检查（复用统一辅助函数：完整则跳过、部分则截断到 512 对齐并返回偏移）
        let start_offset = compute_read_resume_offset(out_file, size);
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
        bar.set_message(format!("读取: {}", part_name));
        if start_offset > 0 {
            bar.set_position(start_offset);
        }

        // 滑动窗口速度计算（10 秒窗口，每 4MB 采样一次）
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        let speed_window_size: u64 = 10;
        let speed_sample_interval: u64 = 4 * 1024 * 1024; // 4MB
        let speed_window: Arc<Mutex<VecDeque<(std::time::Instant, u64)>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(64)));
        let last_speed_sample: Arc<Mutex<u64>> = Arc::new(Mutex::new(start_offset));

        // 流式读取：每个 USB 包写入文件后立即更新进度条
        // 注意：readflash_to_file 内部会在非活跃续传分支按 (addr + start_offset) 重新下发
        // READ_DATA，故此处必须传“分区基址”(addr) 而非已偏移地址，否则会出现双重偏移、
        // 读到分区之外的错误区域（数据损坏）。活跃续传分支只发 ACK，不使用 addr。
        let part_name_clone = part_name.to_string();
        let total = self.readflash_to_file(addr, size, parttype, out_file, start_offset, {
            let bar = bar.clone();
            let speed_window = speed_window.clone();
            let last_speed_sample = last_speed_sample.clone();
            move |bytes_read| {
                bar.set_position(bytes_read);

                // 滑动窗口速度计算
                let now = std::time::Instant::now();
                let prev = *last_speed_sample.lock().unwrap();
                let delta = bytes_read.saturating_sub(prev);
                if delta >= speed_sample_interval || bytes_read == size {
                    {
                        let mut window = speed_window.lock().unwrap();
                        window.push_back((now, bytes_read));
                        *last_speed_sample.lock().unwrap() = bytes_read;

                        // 移除过期的采样点（VecDeque::pop_front 为 O(1)）
                        let deadline = now - std::time::Duration::from_secs(speed_window_size);
                        while window.len() > 2 && window.front().unwrap().0 < deadline {
                            window.pop_front();
                        }

                        // 计算窗口平均速度
                        if window.len() >= 2 {
                            let first = window.front().unwrap();
                            let last = window.back().unwrap();
                            let time_diff = last.0.duration_since(first.0).as_secs_f64();
                            if time_diff > 0.01 {
                                let byte_diff = last.1.saturating_sub(first.1);
                                let speed_mib = (byte_diff as f64 / 1024.0 / 1024.0) / time_diff;
                                bar.set_message(format!(
                                    "读取: {} {:.2} MB/s",
                                    part_name_clone, speed_mib
                                ));
                            }
                        }
                    }
                }
            }
        })?;

        bar.finish_with_message(format!("{} 读取完成 ({} 字节)", part_name, total));
        info!("  已保存到: {}", out_file);
        Ok(())
    }

    /// 读取 super 分区内的动态分区（logical partition）
    ///
    /// 用法: 读取动态分区 "system" 到 "system.img"
    /// 内部通过 super 元数据解析逻辑分区的物理偏移。
    pub fn read_dynamic_partition(
        &mut self,
        logical_part_name: &str,
        out_file: &str,
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
            .find_partition_smart(logical_part_name)
            .ok_or_else(|| format!("在 super 中未找到逻辑分区: {}", logical_part_name))?;

        let phys_addr = super_addr + logical_offset;
        debug!(
            "读取动态分区 {}[{}]: super[0x{:08X}] + offset[0x{:08X}] = 物理地址 0x{:08X}, 大小 {} 字节",
            logical_part_name, actual_name, super_addr, logical_offset, phys_addr, logical_size
        );

        // 4. 断点续传检查：对齐到 512 字节边界（复用统一辅助 compute_read_resume_offset，
        //    与 读取分区 / rl 共用同一“截断对齐 + 已完成跳过”逻辑，避免行为漂移；
        //    且该辅助在 existing >= size 时返回 size，可避免 start_offset 越界导致
        //    后续 readflash_to_file 中 size - start_offset 的 u64 下溢）。
        let start_offset = compute_read_resume_offset(out_file, logical_size);
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
            let resume_path = crate::resume::read_resume_path(out_file);
            let _ = std::fs::remove_file(&resume_path);
        }
        self.readflash_to_file(
            phys_addr,
            logical_size,
            8,
            out_file,
            start_offset,
            move |bytes_read| {
                bar_clone.set_position(bytes_read);
            },
        )?;
        bar.finish_with_message("完成");
        info!("  动态分区 {} 已保存到: {}", logical_part_name, out_file);
        Ok(())
    }
}
