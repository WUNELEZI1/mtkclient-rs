//! DAXFlash 上的分区读写擦操作
//!
//! - `read_gpt`   — 读取并保存 GPT
//! - `find_partition_addr` — 按名称查找分区地址
//! - `read_partition` — 读分区到文件
//! - `write_partition` — 写文件到分区
//! - `erase_partition` — 擦除分区
//! - `write_flash_data` / `cmd_write_data` / `get_packet_length` — 底层写入原语

use log::info;

use crate::da::xflash::{CMD_FORMAT, CMD_MAGIC, DAXFlash, pack3};

use super::gpt::GptInfo;

impl<'a> DAXFlash<'a> {
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
            let num_entries = gpt_info.num_part_entries as u64;
            let entry_size = gpt_info.part_entry_size as u64;
            let needed_len = 512 + num_entries * entry_size;

            info!(
                "  GPT 分区数: {}, 分区项大小: {} 字节, 需要: {} 字节",
                num_entries, entry_size, needed_len
            );

            // 第二步：如果 32KB 不够，再扩展读取完整大小
            if needed_len > initial_len {
                gpt_data = self.readflash_data(0, needed_len)?;
            }
        }
        info!("  读取 GPT 数据: {} 字节", gpt_data.len());

        // 保存原始数据供调试模式使用
        self.last_gpt_data = Some(gpt_data.clone());

        // 写入完整原始数据到文件
        std::fs::write("gpt_full.bin", &gpt_data).expect("写 gpt_full.bin 失败");
        info!("已写入 gpt_full.bin, {} 字节", gpt_data.len());

        // 备份 gpt.bin（对齐 mtkclient 行为）
        std::fs::write("gpt.bin", &gpt_data).expect("写 gpt.bin 失败");
        info!("已写入 gpt.bin, {} 字节", gpt_data.len());

        // 解析 GPT
        super::partition_table::parse_gpt_from_data(&gpt_data)
    }

    /// 查找分区的物理地址和大小（需要 GPT 数据）
    pub(crate) fn find_partition_addr(&mut self, partition: &str) -> Result<(u64, u64), String> {
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;

        let gpt_info = GptInfo::parse(gpt_data)?;

        if let Some(entry) = gpt_info.find_partition(partition) {
            Ok((entry.start_addr, entry.size))
        } else {
            Err(format!("未找到分区: {}", partition))
        }
    }

    /// 解析特殊分区名（boot1/boot2/rpmb 等），返回 (parttype, addr, size)
    /// parttype: 1=boot1, 2=boot2, 3=rpmb, 8=user
    fn resolve_special_partition(&mut self, name: &str) -> Option<(u32, u64, u64)> {
        let emmc = self.get_emmc_info().ok()?;
        let lower = name.to_lowercase();
        match lower.as_str() {
            "boot1" | "emmc_boot1" => {
                if emmc.boot1_size > 0 {
                    Some((1, 0, emmc.boot1_size))
                } else {
                    None
                }
            }
            "boot2" | "emmc_boot2" => {
                if emmc.boot2_size > 0 {
                    Some((2, 0, emmc.boot2_size))
                } else {
                    None
                }
            }
            "rpmb" | "emmc_rpmb" => {
                if emmc.rpmb_size > 0 {
                    Some((3, 0, emmc.rpmb_size))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 读取分区数据到文件
    /// 支持断点续传：检测已有文件大小，从断点继续读取
    /// 流式写入：每个 USB 数据包收到后立即写入磁盘，内存占用极低
    /// 进度显示：使用 indicatif 进度条，每包更新
    pub fn 读取分区(&mut self, 分区名: &str, 输出文件: &str) -> Result<(), String> {
        use indicatif::{ProgressBar, ProgressStyle};
        use std::time::Duration;

        // 解析分区信息（特殊分区或 GPT 分区）
        let (parttype, addr, size) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(分区名) {
                info!(
                    "读取特殊分区 {} (parttype={}), addr=0x{:X}, size={} 字节",
                    分区名, pt, addr, size
                );
                (pt, addr, size)
            } else {
                if self.last_gpt_data.is_none() {
                    self.read_gpt()?;
                }
                let gpt_data = self
                    .last_gpt_data
                    .as_ref()
                    .ok_or_else(|| "无 GPT 数据，请先运行 printgpt".to_string())?;
                let gpt_info = GptInfo::parse(gpt_data)?;
                let entry = gpt_info
                    .find_partition(分区名)
                    .ok_or_else(|| format!("未找到分区: {}", 分区名))?;
                info!(
                    "找到分区 {}，起始地址: 0x{:X}，大小: {} 字节",
                    分区名, entry.start_addr, entry.size
                );
                (8u32, entry.start_addr, entry.size)
            };

        let 输出路径 = std::path::Path::new(输出文件);
        if 输出路径.is_dir() {
            return Err(format!("输出路径是目录，不是文件: {}", 输出文件));
        }
        if let Some(parent) = 输出路径.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录失败 '{}': {}", parent.display(), e))?;
        }

        // 断点续传检查
        let existing_size = if 输出路径.exists() {
            std::fs::metadata(输出文件).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        if existing_size >= size {
            info!("  文件已存在且完整 ({} 字节)，跳过读取", existing_size);
            return Ok(());
        }

        // 对齐到 512 字节边界
        let start_offset = if existing_size > 0 {
            let aligned = (existing_size / 512) * 512;
            if aligned != existing_size {
                info!("  断点续传: 截断到 512 对齐 {} 字节", aligned);
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(输出文件)
                    .map_err(|e| format!("打开文件失败: {}", e))?;
                file.set_len(aligned)
                    .map_err(|e| format!("截断文件失败: {}", e))?;
            }
            info!(
                "  断点续传: 已有 {} 字节 ({}%)，还需读取 {} 字节",
                aligned,
                aligned as f64 / size as f64 * 100.0,
                size - aligned
            );
            aligned
        } else {
            0
        };

        // 创建进度条（滑动窗口平均速度，避免瞬时速度波动）
        let bar = ProgressBar::new(size);
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
        bar.enable_steady_tick(Duration::from_millis(500));
        bar.set_position(start_offset);

        // 滑动窗口速度计算（10 秒窗口，每 1MB 采样一次）
        use std::sync::{Arc, Mutex};
        let 速度窗口大小: u64 = 10;
        let 速度采样间隔: u64 = 1 * 1024 * 1024; // 1MB
        let 速度窗口: Arc<Mutex<Vec<(std::time::Instant, u64)>>> =
            Arc::new(Mutex::new(Vec::with_capacity(64)));
        let 上次速度采样: Arc<Mutex<u64>> = Arc::new(Mutex::new(start_offset));

        // 流式读取：每个 USB 包写入文件后立即更新进度条
        let read_addr = addr + start_offset;
        let 分区名clone = 分区名.to_string();
        let total =
            self.readflash_to_file(read_addr, size, parttype, 输出文件, start_offset, {
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
                            窗口.push((now, bytes_read));
                            *上次速度采样.lock().unwrap() = bytes_read;

                            // 移除过期的采样点
                            let 截止 = now - std::time::Duration::from_secs(速度窗口大小);
                            while 窗口.len() > 2 && 窗口[0].0 < 截止 {
                                窗口.remove(0);
                            }

                            // 计算窗口平均速度
                            if 窗口.len() >= 2 {
                                let 首次 = &窗口[0];
                                let 末次 = &窗口[窗口.len() - 1];
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

    /// 写入文件到分区（带校验）
    pub fn 写入分区带校验(
        &mut self, 分区名: &str, 输入文件: &str
    ) -> Result<(), String> {
        // 先写入
        self.写入分区(分区名, 输入文件)?;

        // 读取回来校验
        info!("  开始校验写入数据...");
        let 原始数据 = std::fs::read(输入文件).map_err(|e| format!("读取原始文件失败: {}", e))?;

        // 读取刚写入的数据
        let gpt_data = self
            .last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())?;
        let gpt_info = GptInfo::parse(gpt_data)?;
        let entry = gpt_info
            .find_partition(分区名)
            .ok_or_else(|| format!("未找到分区: {}", 分区名))?;

        let 验证数据 = self.readflash_data(entry.start_addr, 原始数据.len() as u64)?;

        if 原始数据 == 验证数据 {
            info!("  校验通过 ✓");
            Ok(())
        } else {
            Err("校验失败：写入数据与原始数据不匹配".to_string())
        }
    }

    /// 写入文件到分区
    /// 对齐 Python writeflash (xflash_lib.py:writeflash)
    /// 协议: cmd_write_data → 循环分包写入 [0x0(4B)][checksum(4B)][data] → CC_OPTIONAL_DOWNLOAD_ACT → status
    /// 增加校验：文件大小不能超过分区大小，超过时报错。
    pub fn 写入分区(&mut self, 分区名: &str, 输入文件: &str) -> Result<(), String> {
        info!("写入文件 {} 到分区 {}...", 输入文件, 分区名);

        // 读取文件
        let 文件数据 = std::fs::read(输入文件).map_err(|e| format!("无法读取文件: {}", e))?;
        let 文件大小 = 文件数据.len();

        // 先尝试解析特殊分区（boot1/boot2/rpmb）
        let (parttype, 地址, 分区大小) =
            if let Some((pt, addr, size)) = self.resolve_special_partition(分区名) {
                info!("写入特殊分区 {} (parttype={})", 分区名, pt);
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

        // 校验：文件大小不能超过分区大小
        if 文件大小 as u64 > 分区大小 {
            return Err(format!(
                "文件大小 ({}) 超过分区 {} 容量 ({} 字节)，写入被拒绝",
                文件大小, 分区名, 分区大小
            ));
        }
        if 文件大小 as u64 == 分区大小 {
            info!("  文件大小与分区容量完全匹配");
        } else {
            info!("  文件大小: {} 字节, 分区容量: {} 字节", 文件大小, 分区大小);
        }

        let mut 数据 = 文件数据;
        // 对齐到 512 字节（Python: 如果长度不是 512 的倍数，补零）
        let 填充 = if 数据.len() % 512 != 0 {
            512 - (数据.len() % 512)
        } else {
            0
        };
        if 填充 > 0 {
            数据.resize(数据.len() + 填充, 0);
        }
        self.write_flash_data(地址, &数据, 1, parttype)?;

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
        const STATUS_COMPLETE: u32 = 0x40040005;
        const STATUS_CONTINUE: u32 = 0x40040004;

        let mut status = self.status()?;
        while status == STATUS_CONTINUE {
            // STATUS_CONTINUE 包含等待时间（毫秒）
            let wait_ms = self.status()?;
            std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
            let _ = self.ack(); // AckResult 的 Debug 输出已满足日志需求
            status = self.status()?;
        }

        if status != STATUS_COMPLETE {
            return Err(format!("擦除失败: status=0x{:08X}", status));
        }

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
