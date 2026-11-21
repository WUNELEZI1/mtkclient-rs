//! GPT partition table commands
//!
//! - `printgpt`         — moved to `ops::printgpt` (capability-driven, see exec module)
//! - `cmd_read_gpt`     — read raw GPT data into a directory
//! - `cmd_read_all`     — read all partitions into a directory
//! - `cmd_print_scatter`— print scatter to screen and save to file
//!
//! Private helpers:
//! - `print_gpt_table`  — formatted table output

use crate::color::Colorize;
use log::{info, trace, warn};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

mod emmc;
mod table;

/// Table rendering reused by the ops layer (keeps output consistent with the legacy `printgpt` command).
pub(crate) fn print_gpt_table_public(
    data: &[u8],
    boot1_size: u64,
    boot2_size: u64,
    super_metadata: Option<&crate::partition::lp::SuperMetadata>,
) {
    table::print_gpt_table(data, boot1_size, boot2_size, super_metadata);
}

/// Read raw GPT data to the given path
///
/// If `path` has a file extension (e.g. .bin/.img), save directly to that file;
/// otherwise treat it as a directory and create `gpt.bin` inside it.
pub fn cmd_read_gpt(
    da: &mut DAXFlash,
    path: &str,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    // Determine whether path is a file or directory: a common extension means it is a file
    let output = if path.ends_with(".bin") || path.ends_with(".img") || path.ends_with(".dat") {
        path.to_string()
    } else {
        std::fs::create_dir_all(path).map_err(|e| format!("创建目录失败: {}", e))?;
        format!("{}/gpt.bin", path)
    };

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?;
    DAXFlash::save_gpt_cache_file(&output, gpt_data).map_err(|e| format!("写入失败: {}", e))?;
    info!(
        "{}",
        format!("GPT 已保存: {} ({} 字节)", output, gpt_data.len()).green()
    );

    if log_level >= 2 {
        table::print_gpt_table(gpt_data, 0, 0, None);
    }

    Ok(())
}

/// Write a GPT image back to the device (eMMC, primary GPT area only: LBA0 protective MBR + LBA1 header + partition entries)
///
/// Counterpart to `cmd_read_gpt`: `r gpt <dir>` produces `<dir>/gpt.bin`, which this command writes back.
/// Entry CRC32 and header CRC32 are recomputed before writing so the GPT is valid (SPFT can read it directly).
/// The write range is the actual image length (LBA0 + primary GPT header + entry table); the backup GPT is not written
/// because the image lacks the tail-of-disk area. The primary GPT is what the Bootloader reads first at startup, satisfying GPT modification/recovery needs.
pub fn cmd_write_gpt(da: &mut DAXFlash, file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut data =
        std::fs::read(file).map_err(|e| format!("读取 GPT 文件失败 '{}': {}", file, e))?;
    let gpt_info =
        crate::partition::GptInfo::parse(&data).map_err(|e| format!("GPT 文件无效: {}", e))?;
    // Copy fields out to release the immutable borrow of data and avoid conflict with the later &mut data (E0502)
    let base_offset = gpt_info.base_offset;
    let part_entry_start_lba = gpt_info.part_entry_start_lba;
    let num_part_entries = gpt_info.num_part_entries;
    let part_entry_size = gpt_info.part_entry_size;
    let header_size = gpt_info.header_size;

    // Recompute CRC before writing so the device/SPFT read a valid GPT
    // Note: the immutable borrow of gpt_info ends once its fields are copied out (NLL); no explicit drop needed
    recompute_gpt_crc(
        &mut data,
        base_offset,
        part_entry_start_lba,
        num_part_entries,
        part_entry_size,
        header_size,
    );

    info!("写入 GPT 到设备 ({} 字节, 主 GPT 区域)...", data.len());
    let mut cursor = std::io::Cursor::new(data.clone());
    da.write_flash_data_stream(0, &mut cursor, data.len() as u64, 1, 8, 0, file)
        .map_err(|e| format!("写入 GPT 失败: {}", e))?;

    info!("{}", format!("GPT 已写入: {}", file).green());
    Ok(())
}

/// Recompute GPT header CRC32 and partition entry CRC32 (modifies the buffer in place)
///
/// - Entry CRC: covers `part_entry_start_lba*512 .. + num*entry_size`
/// - Header CRC: zero the header CRC field (offset 16..20) first, then compute over `header_size` bytes
fn recompute_gpt_crc(
    buf: &mut [u8],
    base_offset: usize,
    part_entry_start_lba: u64,
    num_part_entries: u32,
    part_entry_size: u32,
    header_size: u32,
) {
    let base = base_offset;

    // Entry CRC (stored in the header at base+88..92)
    let ent_off = (part_entry_start_lba * 512) as usize;
    let ent_len = (num_part_entries * part_entry_size) as usize;
    if ent_off + ent_len <= buf.len() && ent_len > 0 {
        let crc = crate::partition::gpt::crc32_ieee(&buf[ent_off..ent_off + ent_len]);
        buf[base + 88..base + 92].copy_from_slice(&crc.to_le_bytes());
    }

    // Header CRC (zero the CRC field before computing)
    let hdr_len = header_size as usize;
    if base + hdr_len <= buf.len() && hdr_len >= 20 {
        buf[base + 16..base + 20].copy_from_slice(&0u32.to_le_bytes());
        let crc = crate::partition::gpt::crc32_ieee(&buf[base..base + hdr_len]);
        buf[base + 16..base + 20].copy_from_slice(&crc.to_le_bytes());
    }
}

/// Read all partitions into a directory (supports --skip to omit specified partitions)
pub fn cmd_read_all(
    da: &mut DAXFlash,
    dir: &str,
    app_config: &AppConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    // Parse the --skip partition list
    // Note: trailing args of clap (Vec, allow_hyphen_values=true) greedily consume the root-level `--skip`
    // flag after a command as command arguments, leaving app_config.skip_partitions empty. So besides reading
    // the root flag, also scan app_config.cmd_args for `--skip <val>` / `--skip=<val>` (same as reboot --via).
    let mut skip_raw: Option<String> = app_config.skip_partitions.clone();
    if skip_raw.is_none() {
        // `--skip <val>`
        for w in app_config.cmd_args.windows(2) {
            if w[0] == "--skip" {
                skip_raw = Some(w[1].clone());
                break;
            }
        }
        // `--skip=<val>`
        if skip_raw.is_none() {
            for a in &app_config.cmd_args {
                if let Some(v) = a.strip_prefix("--skip=") {
                    skip_raw = Some(v.to_string());
                    break;
                }
            }
        }
    }
    let skip_set: std::collections::HashSet<String> = match skip_raw {
        Some(skip_str) => {
            let set: std::collections::HashSet<String> = skip_str
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            if !set.is_empty() {
                info!("跳过分区: {}", skip_str);
            }
            set
        }
        None => std::collections::HashSet::new(),
    };

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::partition::GptInfo::parse(&gpt_data)?;
    let mut read_count = 0usize; // actually read/resumed successfully (for the summary)
    let mut done_count = 0usize; // already present and complete, skipped this run without re-reading
    let mut idx = 0usize; // [n/total] continuous counter (includes DONE skips so rerun numbering stays continuous)

    // Pre-count --skip partitions to avoid a changing denominator if incremented inside the loop
    let skip_count = gpt_info
        .iter_partitions()
        .filter(|e| skip_set.contains(&e.name.to_lowercase()))
        .count();
    let total = gpt_info.partitions().len() - skip_count;

    for entry in gpt_info.iter_partitions() {
        // Skip specified partitions (case-insensitive comparison)
        if skip_set.contains(&entry.name.to_lowercase()) {
            info!("  [SKIP] {} (0x{:X})", entry.name, entry.size);
            continue; // skip_count is pre-counted, so do not increment here
        }

        let output = format!("{}/{}.img", dir, entry.name);

        // Resume / already-done skip:
        // 1) Remove any leftover `.resume` active-read marker — on a cross-process rerun the device is no longer
        //    pushing the old data stream, so resend READ_DATA to resume from the breakpoint instead of ACKing to
        //    continue the active stream (avoids resume failure).
        // 2) compute_read_resume_offset: file complete -> returns size (skip); partially present -> truncates to a 512-aligned boundary and returns the offset (resume).
        let _ = std::fs::remove_file(crate::resume::read_resume_path(&output));
        let start_offset = crate::partition::io::compute_read_resume_offset(&output, entry.size);
        if start_offset >= entry.size {
            info!(
                "  [DONE] {} 已存在且完整 ({} 字节)，跳过",
                entry.name, entry.size
            );
            idx += 1;
            done_count += 1;
            continue;
        }

        idx += 1;
        info!(
            "  [{}/{}] 读取 {} (0x{:X} @ 0x{:X}){}",
            idx,
            total,
            entry.name,
            entry.size,
            entry.start_addr,
            if start_offset > 0 {
                format!(" 断点续传 @0x{:X}", start_offset)
            } else {
                String::new()
            }
        );

        // Chunked partition read with a progress bar (matches the io::cmd_read single-partition experience;
        // parttype=8 corresponds to the GPT physical partition; streaming to file avoids loading a large partition entirely into memory)
        use crate::progress::{ProgressBar, ProgressStyle};
        let bar = if crate::usb::log::QUIET_USB_READ.load(std::sync::atomic::Ordering::Relaxed) {
            ProgressBar::hidden()
        } else {
            ProgressBar::new(entry.size)
        };
        bar.set_style(
            ProgressStyle::with_template(
                "  {spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
                 {binary_bytes}/{binary_total_bytes} ({percent}%) {msg}",
            )
            .unwrap()
            .progress_chars("█▓░"),
        );
        bar.set_message(format!("读取: {}", entry.name));
        if start_offset > 0 {
            bar.set_position(start_offset);
        }
        // On resume, continue reading from start_offset.
        // Note: readflash_to_file internally resends READ_DATA at (addr + start_offset) in the inactive-resume branch,
        // so pass the "partition base address" entry.start_addr (not the already-offset address) to avoid double offsetting.
        da.readflash_to_file(entry.start_addr, entry.size, 8, &output, start_offset, {
            let bar = bar.clone();
            move |bytes_read| {
                bar.set_position(bytes_read);
            }
        })
        .map_err(|e| format!("读取 {} 失败: {}", entry.name, e))?;
        bar.finish_and_clear();
        info!("{}", format!("  {} -> {}", entry.name, output).green());
        read_count += 1;
    }

    info!(
        "{}",
        format!(
            "分区读取完成: {} 实际读取/续传, {} 已存在跳过(未重读), {} 指定跳过, 目录={}",
            read_count, done_count, skip_count, dir
        )
        .green()
    );
    Ok(())
}

/// Read all `<partition name>`.img or `<partition name>`.bin files from a directory and write them back to their partitions
pub fn cmd_write_all(
    da: &mut DAXFlash,
    dir: &str,
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;

    if da.get_last_gpt_data().is_err() {
        da.read_gpt().map_err(|e| format!("GPT 读取失败: {}", e))?;
    }

    let gpt_data = da.get_last_gpt_data()?.clone();
    let gpt_info = crate::partition::GptInfo::parse(&gpt_data)?;

    let mut write_count = 0usize;
    let mut skip_count = 0usize;

    for entry in gpt_info.iter_partitions() {
        // Prefer .img, then .bin
        let img_path = format!("{}/{}.img", dir, entry.name);
        let bin_path = format!("{}/{}.bin", dir, entry.name);
        let input = if std::path::Path::new(&img_path).exists() {
            img_path
        } else if std::path::Path::new(&bin_path).exists() {
            bin_path
        } else {
            trace!("  跳过 {} (文件不存在)", entry.name);
            skip_count += 1;
            continue;
        };

        info!(
            "  [{}/{}] 写入 {} <- {} (0x{:X} 字节)",
            write_count + 1,
            gpt_info.partitions().len(),
            entry.name,
            input,
            entry.size
        );
        if let Err(e) = da.write_partition(&entry.name, &input) {
            warn!("  写入 {} 失败: {}", entry.name, e);
            continue;
        }

        if verify {
            let orig = std::fs::read(&input).map_err(|e| format!("读取 {} 失败: {}", input, e))?;
            let verify = da.readflash_data(entry.start_addr, orig.len() as u64)?;
            if orig != verify {
                warn!("  {} 校验失败", entry.name);
            } else {
                info!("  {} 校验通过 ✓", entry.name);
            }
        }
        write_count += 1;
    }

    info!(
        "{}",
        format!(
            "分区写入完成: {} 成功, {} 跳过, 目录={}",
            write_count, skip_count, dir
        )
        .green()
    );
    Ok(())
}
