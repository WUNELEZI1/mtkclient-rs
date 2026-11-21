//! Partition reads (single partition / dynamic partition)

use crate::da::xflash::DAXFlash;
use crate::partition::io::compute_read_resume_offset;
use crate::usb::log::QUIET_USB_READ;
use log::{debug, info};
use std::sync::atomic::Ordering;

impl<'a> DAXFlash<'a> {
    /// Reads partition data into a file.
    /// Resumable: detects the existing file size and continues from the resume point.
    /// Streaming: each USB packet is written to disk immediately, so memory usage is minimal.
    /// Progress: uses the in-house progress bar (src/progress.rs), updated per packet.
    pub fn read_partition(&mut self, part_name: &str, out_file: &str) -> Result<(), String> {
        use crate::progress::{ProgressBar, ProgressStyle};

        // Resolve partition info (special partition or GPT partition)
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
                // find_partition_addr already handles the GPT physical partition -> super dynamic partition fallback
                let (addr, size) = self.find_partition_addr(part_name)?;
                info!(
                    "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
                    part_name, addr, size
                );
                (8u32, addr, size)
            };

        // Resolve the output path: if it is a directory (existing directory / trailing path
        // separator / no extension and does not exist), write `<partition>`.img inside it,
        // matching the rl command so "output path is a directory" is not misreported.
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

        // Resume check (reuses the shared helper: skip when complete, truncate to 512 alignment and return the offset when partial)
        let start_offset = compute_read_resume_offset(out_file, size);
        if start_offset >= size {
            info!(
                "{}",
                crate::i18n::tr("log_skip_existing", &[&start_offset.to_string()])
            );
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

        // Create the progress bar (output to stderr, same stream as logs, to avoid visual interleaving)
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

        // Sliding-window speed calculation (10s window, sampled every 4MB)
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        let speed_window_size: u64 = 10;
        let speed_sample_interval: u64 = 4 * 1024 * 1024; // 4MB
        let speed_window: Arc<Mutex<VecDeque<(std::time::Instant, u64)>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(64)));
        let last_speed_sample: Arc<Mutex<u64>> = Arc::new(Mutex::new(start_offset));

        // Streaming read: update the progress bar immediately after each USB packet is written
        // Note: on the inactive-resume branch, readflash_to_file re-issues READ_DATA with
        // (addr + start_offset), so the "partition base address" (addr) must be passed here,
        // not a pre-offset address; otherwise the offset is applied twice and data outside
        // the partition is read (corruption). The active-resume branch only sends ACKs and
        // does not use addr.
        let part_name_clone = part_name.to_string();
        let total = self.readflash_to_file(addr, size, parttype, out_file, start_offset, {
            let bar = bar.clone();
            let speed_window = speed_window.clone();
            let last_speed_sample = last_speed_sample.clone();
            move |bytes_read| {
                bar.set_position(bytes_read);

                // Sliding-window speed calculation
                let now = std::time::Instant::now();
                let prev = *last_speed_sample.lock().unwrap();
                let delta = bytes_read.saturating_sub(prev);
                if delta >= speed_sample_interval || bytes_read == size {
                    {
                        let mut window = speed_window.lock().unwrap();
                        window.push_back((now, bytes_read));
                        *last_speed_sample.lock().unwrap() = bytes_read;

                        // Drop expired samples (VecDeque::pop_front is O(1))
                        let deadline = now - std::time::Duration::from_secs(speed_window_size);
                        while window.len() > 2 && window.front().unwrap().0 < deadline {
                            window.pop_front();
                        }

                        // Compute the window average speed
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
        info!("{}", crate::i18n::tr("log_saved_to", &[out_file]));
        Ok(())
    }

    /// Reads a dynamic partition (logical partition) inside the super partition.
    ///
    /// Usage: read dynamic partition "system" into "system.img".
    /// Resolves the logical partition's physical offset through super metadata.
    pub fn read_dynamic_partition(
        &mut self,
        logical_part_name: &str,
        out_file: &str,
    ) -> Result<(), String> {
        // 1. Locate the super physical partition address
        let (super_addr, _super_size) = self.find_partition_addr("super")?;

        // 2. Ensure super metadata is loaded (unified entry: super -> backup -> metadata)
        if self.super_metadata.is_none() {
            self.ensure_super_metadata()
                .map_err(|e| format!("加载 super 元数据失败: {}", e))?;
        }

        // 3. Look up the logical partition in super metadata (A/B slot handled automatically)
        let meta = self.super_metadata.as_ref().ok_or("super 元数据不可用")?;
        let (actual_name, logical_offset, logical_size) = meta
            .find_partition_smart(logical_part_name)
            .ok_or_else(|| format!("在 super 中未找到逻辑分区: {}", logical_part_name))?;

        let phys_addr = super_addr + logical_offset;
        debug!(
            "读取动态分区 {}[{}]: super[0x{:08X}] + offset[0x{:08X}] = 物理地址 0x{:08X}, 大小 {} 字节",
            logical_part_name, actual_name, super_addr, logical_offset, phys_addr, logical_size
        );

        // 4. Resume check: align to a 512-byte boundary (reuse the shared helper
        //    compute_read_resume_offset, sharing the same "truncate-align + skip if done"
        //    logic with partition reads / rl to avoid behavior drift; it also returns
        //    size when existing >= size, preventing start_offset overflow and the
        //    subsequent u64 underflow in readflash_to_file's size - start_offset).
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
            info!(
                "{}",
                crate::i18n::tr("log_skip_existing", &[&start_offset.to_string()])
            );
            return Ok(());
        }

        // 5. Create the progress bar and stream the read
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
        // Clean up the resume file only when the file is completely absent or empty.
        // (Keep the resume file: if the device is still mid-stream, the active_resume
        //  path resumes via ACKs; if the device has disconnected, a failed active_resume
        //  ACK falls back to the inactive path and issues a new READ_DATA.)
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
