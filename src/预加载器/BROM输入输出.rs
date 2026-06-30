//! BROM IO 操作：DA 加载/跳转 + ME_ID/SOC_ID 查询
//!
//! - `send_da`    — 发送 Download Agent（含 64 字节分块 + 重试 + ZLP）
//! - `jump_da`    — 跳转到 DA（含 5 次重试，每次重试 clear_halt + flush）
//! - `jump_bl`    — 跳转到 Bootloader
//! - `get_me_id`  — 读取 ME_ID
//! - `get_soc_id` — 读取 SOC_ID

use super::核心::Preloader;
use log::trace;
use std::time::Duration;

const DA_UPLOAD_TIMEOUT_MS: u64 = 5000;
const DA_UPLOAD_CHUNK: usize = 64;
const DA_UPLOAD_RETRY: u32 = 3;
const DA_UPLOAD_RETRY_DELAY_MS: u64 = 10;
const DA_UPLOAD_BURST_DELAY_MS: u64 = 2;
const DA_UPLOAD_BURST_BLOCKS: u32 = 256;
const DA_POST_UPLOAD_DELAY_MS: u64 = 20;
const JUMP_DA_MAX_ATTEMPT: u32 = 5;
const JUMP_DA_FIRST_DELAY_MS: u64 = 50;
const JUMP_DA_RETRY_DELAY_MS: u64 = 100;
const JUMP_DA_RETRY_QUIET_MS: u64 = 30;
const JUMP_BL_POST_DELAY_MS: u64 = 50;

impl Preloader {
    /// SEND_DA: 发送 Download Agent 到设备
    /// 对齐 Python mtkclient 2.0.1 的实现
    pub fn send_da(
        &mut self,
        address: u32,
        size: u32,
        sig_len: u32,
        dadata: &[u8],
    ) -> Result<bool, String> {
        trace!(
            "SEND_DA: addr=0x{:08X}, size={}, sig_len={}",
            address, size, sig_len
        );

        // 1. echo(0xD7) 命令
        if !self.echo_1byte(0xD7)? {
            return Err("SEND_DA: echo 0xD7 不匹配".into());
        }

        // 2. 发送参数
        if !self.echo_4byte(address)? {
            return Err("SEND_DA: echo addr 不匹配".into());
        }
        if !self.echo_4byte(size)? {
            return Err("SEND_DA: echo size 不匹配".into());
        }
        if !self.echo_4byte(sig_len)? {
            return Err("SEND_DA: echo sig_len 不匹配".into());
        }

        // 3. rword() 读状态
        let status = self.rword()?;
        trace!("SEND_DA status: {:04X}", status);
        if status > 0xFF {
            return Err(format!("SEND_DA status error: {:04X}", status));
        }
        if status == 0x1D0D {
            return Err("SLA required".into());
        }

        // 4. 上传数据
        trace!(
            "[UPLOAD] sending {} bytes in chunks of {}",
            dadata.len(),
            DA_UPLOAD_CHUNK
        );

        // 4a. 清除端点状态（对应 pyUSB 自动处理的 clear_halt）
        if self.device.is_libusb() {
            trace!("[UPLOAD] clear_halt_out before upload");
            let _ = self.device.clear_halt_out();
        }

        // 4b. 设置超时
        let orig_timeout = self.device.get_timeout();
        self.device
            .set_timeout(Duration::from_millis(DA_UPLOAD_TIMEOUT_MS));

        // 4c. 发送数据（64 字节块，对齐 Python upload_data）
        let mut pos = 0;
        let mut chunk_count = 0;

        while pos < dadata.len() {
            let end = (pos + DA_UPLOAD_CHUNK).min(dadata.len());
            // 重试机制：设备可能在处理时短暂 stall
            let mut attempt = 0;
            loop {
                attempt += 1;
                match self.device.write(&dadata[pos..end]) {
                    Ok(_) => break,
                    Err(e) => {
                        if attempt >= DA_UPLOAD_RETRY {
                            return Err(format!("upload_data write (pos={}): {}", pos, e));
                        }
                        trace!(
                            "[UPLOAD] write fail at pos={} (attempt {}): {}, retrying",
                            pos, attempt, e
                        );
                        if self.device.is_libusb() {
                            let _ = self.device.clear_halt_out();
                        }
                        std::thread::sleep(Duration::from_millis(DA_UPLOAD_RETRY_DELAY_MS));
                    }
                }
            }
            pos = end;
            chunk_count += 1;
            if chunk_count % DA_UPLOAD_BURST_BLOCKS == 0 {
                std::thread::sleep(Duration::from_millis(DA_UPLOAD_BURST_DELAY_MS));
            }
        }

        // 4d. ZLP
        trace!("[UPLOAD] sending ZLP");
        self.device
            .write(&[])
            .map_err(|e| format!("upload_data ZLP: {}", e))?;

        // 4e. 等待设备处理（对齐 Python: time.sleep(0.035)）
        std::thread::sleep(Duration::from_millis(DA_POST_UPLOAD_DELAY_MS));

        // 4f. 恢复超时
        self.device.set_timeout(orig_timeout);

        // 5. 读校验和 + 状态
        let checksum = self.rword()?;
        let status2 = self.rword()?;
        trace!(
            "SEND_DA checksum: {:04X}, status2: {:04X}",
            checksum, status2
        );

        Ok(true)
    }

    /// JUMP_DA: 跳转到 Download Agent
    /// Python: echo(JUMP_DA) → usbwrite(pack(">I", addr)) → rdword() → rword()
    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        let mut last_err = String::new();
        for attempt in 1..=JUMP_DA_MAX_ATTEMPT {
            if attempt == 1 {
                std::thread::sleep(Duration::from_millis(JUMP_DA_FIRST_DELAY_MS));
            } else {
                std::thread::sleep(Duration::from_millis(JUMP_DA_RETRY_DELAY_MS));
            }

            // 每次重试前清理 USB 端点
            if self.device.is_libusb() {
                let _ = self.device.clear_halt_in();
                let _ = self.device.clear_halt_out();
            }
            self.flush_input();

            if !self.echo_1byte(0xD5)? {
                last_err = "jump_da: echo 0xD5 不匹配".to_string();
                trace!("[JUMP_DA] attempt {}: echo 0xD5 不匹配，重试", attempt);
                continue;
            }
            // Python: usbwrite(pack(">I", addr)) — 大端
            if let Err(e) = self.device.write(&addr.to_be_bytes()) {
                last_err = format!("jump_da write addr: {}", e);
                trace!(
                    "[JUMP_DA] attempt {}: write addr 失败: {}，重试",
                    attempt, e
                );
                self.flush_input();
                if self.device.is_libusb() {
                    let _ = self.device.clear_halt_in();
                    let _ = self.device.clear_halt_out();
                }
                std::thread::sleep(Duration::from_millis(JUMP_DA_RETRY_QUIET_MS));
                continue;
            }
            // Python: rdword() — 大端回读
            let mut echo = [0u8; 4];
            match self.device.read_exact(&mut echo) {
                Ok(_) => {
                    let resaddr = u32::from_be_bytes(echo);
                    if resaddr != addr {
                        return Err(format!(
                            "jump_da addr mismatch: expected {:08X}, got {:08X}",
                            addr, resaddr
                        ));
                    }
                    let mut st = [0u8; 2];
                    self.device
                        .read_exact(&mut st)
                        .map_err(|e| format!("jump_da status: {}", e))?;
                    let status = u16::from_be_bytes(st);
                    // Python v2.1.4.1: time.sleep(0.1) after rword() — fix rare timing issue
                    std::thread::sleep(Duration::from_millis(JUMP_BL_POST_DELAY_MS));
                    trace!("jump_da status: {:04X}", status);
                    return Ok(status == 0);
                }
                Err(e) => {
                    last_err = format!("jump_da echo: {}", e);
                    trace!("[JUMP_DA] attempt {}: read echo 失败: {}，重试", attempt, e);
                    self.flush_input();
                    if self.device.is_libusb() {
                        let _ = self.device.clear_halt_in();
                        let _ = self.device.clear_halt_out();
                    }
                    std::thread::sleep(Duration::from_millis(JUMP_DA_RETRY_QUIET_MS));
                    continue;
                }
            }
        }
        Err(last_err)
    }

    /// JUMP_BL: 跳转到 Bootloader
    /// Python: echo(JUMP_BL) → rword() → if <=0xFF → rword() → if <=0xFF → True
    pub fn jump_bl(&mut self) -> Result<bool, String> {
        if !self.echo_1byte(0xD6)? {
            return Err("jump_bl: echo 0xD6 不匹配".into());
        }
        let status = self.rword()?;
        trace!("jump_bl status: {:04X}", status);
        if status <= 0xFF {
            let status2 = self.rword()?;
            trace!("jump_bl status2: {:04X}", status2);
            Ok(status2 <= 0xFF)
        } else {
            Ok(false)
        }
    }

    /// 获取 ME_ID (0xE1)
    pub fn get_me_id(&mut self) -> Result<Vec<u8>, String> {
        if !self.echo_1byte(0xFE)? {
            return Err("get_me_id: sync FE 失败".into());
        }
        if !self.echo_1byte(0xE1)? {
            return Err("get_me_id: echo 0xE1 失败".into());
        }
        let mut len_buf = [0u8; 4];
        self.device.read_exact(&mut len_buf)?;
        let length = u32::from_be_bytes(len_buf) as usize;
        let mut data = vec![0u8; length];
        self.device.read_exact(&mut data)?;
        let _status = self.rword()?;
        trace!("ME_ID: {:02X?}", data);
        Ok(data)
    }

    /// 获取 SOC_ID (0xE7)
    pub fn get_soc_id(&mut self) -> Result<Vec<u8>, String> {
        if !self.echo_1byte(0xFE)? {
            return Err("get_soc_id: sync FE 失败".into());
        }
        if !self.echo_1byte(0xE7)? {
            return Err("get_soc_id: echo 0xE7 失败".into());
        }
        let mut len_buf = [0u8; 4];
        self.device.read_exact(&mut len_buf)?;
        let length = u32::from_be_bytes(len_buf) as usize;
        let mut data = vec![0u8; length];
        self.device.read_exact(&mut data)?;
        let _status = self.rword()?;
        trace!("SOC_ID: {:02X?}", data);
        Ok(data)
    }
}
