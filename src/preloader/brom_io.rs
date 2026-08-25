//! BROM IO 操作：DA 加载/跳转 + ME_ID/SOC_ID 查询
//!
//! - `send_da`    — 发送 Download Agent（动态 EP 包大小分块 + 重试 + ZLP）
//! - `jump_da`    — 跳转到 DA（含 5 次重试，每次重试 clear_halt + flush）
//! - `jump_bl`    — 跳转到 Bootloader
//! - `get_me_id`  — 读取 ME_ID
//! - `get_soc_id` — 读取 SOC_ID

use super::core::Preloader;
use log::{info, trace};
use std::time::{Duration, Instant};

const DA_UPLOAD_TIMEOUT_MS: u64 = 2000;
const DA_UPLOAD_RETRY: u32 = 5;
const DA_UPLOAD_RETRY_DELAY_MS: u64 = 50;
const JUMP_DA_MAX_ATTEMPT: u32 = 5;

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
        // 对齐 Python mtk_preloader.py: echo() 将 int 转为 pack(">I")，发送 4 字节
        // Preloader 串口模式下设备期望 4 字节命令，1 字节会导致超时
        if !self.echo_4byte(0xD7)? {
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
        let upload_start = Instant::now();
        // mtkclient 2.1.4.1 优化：动态使用 EP_OUT.wMaxPacketSize 替代固定 64B
        // 保留最小值 64，防止端点报告异常值时出问题
        let chunk_size = (self.device.out_ep_max_packet_size() as usize).max(64);
        trace!(
            "[UPLOAD] sending {} bytes, chunk={}",
            dadata.len(),
            chunk_size
        );

        // 4a. 设置超时
        let orig_timeout = self.device.get_timeout();
        self.device
            .set_timeout(Duration::from_millis(DA_UPLOAD_TIMEOUT_MS));

        // 4b. 发送数据（动态 chunk，末尾自动适配）
        let mut pos = 0;
        while pos < dadata.len() {
            let end = (pos + chunk_size).min(dadata.len());
            let mut attempt = 0;
            loop {
                attempt += 1;
                match self.device.write(&dadata[pos..end]) {
                    Ok(_) => break,
                    Err(e) => {
                        if attempt >= DA_UPLOAD_RETRY {
                            self.device.set_timeout(orig_timeout);
                            return Err(format!("upload_data write (pos={}): {}", pos, e));
                        }
                        trace!(
                            "[UPLOAD] write fail at pos={} (attempt {}): {}, retrying",
                            pos, attempt, e
                        );
                        std::thread::sleep(Duration::from_millis(DA_UPLOAD_RETRY_DELAY_MS));
                    }
                }
            }
            pos = end;
        }

        // 4c. ZLP
        trace!("[UPLOAD] sending ZLP");
        self.device
            .write(&[])
            .map_err(|e| format!("upload_data ZLP: {}", e))?;

        // 4d. 对齐 Python: ZLP 后 35ms 延迟，给设备处理数据
        std::thread::sleep(Duration::from_millis(35));

        // 4e. 恢复超时
        self.device.set_timeout(orig_timeout);

        // 5. 读校验和 + 状态
        let checksum = self.rword()?;
        let status2 = self.rword()?;
        trace!(
            "SEND_DA checksum: {:04X}, status2: {:04X}",
            checksum, status2
        );
        let upload_elapsed = upload_start.elapsed();
        info!(
            "SEND_DA 上传完成: {} bytes, 耗时 {:.1}ms",
            dadata.len(),
            upload_elapsed.as_secs_f64() * 1000.0
        );

        Ok(true)
    }

    /// JUMP_DA: 跳转到 Download Agent
    /// 协议: echo(0xD5) → echo(addr) → write(addr) → [0x00...] → 0xC0
    /// 第二个 addr 触发跳转，BROM 可能发 0~2 个 0x00 填充，然后 DA 发 0xC0 同步
    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        let mut last_err = String::new();
        for attempt in 1..=JUMP_DA_MAX_ATTEMPT {
            self.flush_input_poll(Duration::from_millis(5), 40).ok();

            if !self.echo_1byte(0xD5)? {
                last_err = "jump_da: echo 0xD5 不匹配".to_string();
                trace!("[JUMP_DA] attempt {}: echo 0xD5 不匹配，重试", attempt);
                continue;
            }
            // 第一个 addr：BROM 正常回显
            if !self.echo_4byte(addr)? {
                last_err = "jump_da echo addr(1): mismatch".to_string();
                trace!(
                    "[JUMP_DA] attempt {}: 第一次 echo addr 不匹配，重试",
                    attempt
                );
                self.flush_input();
                continue;
            }
            // 第二个 addr：触发跳转
            self.device
                .write(&addr.to_be_bytes())
                .map_err(|e| format!("jump_da write addr(2): {}", e))?;

            // 等待 DA 的 0xC0 同步信号（跳过 0x00 填充字节）
            let orig_timeout = self.device.get_timeout();
            self.device.set_timeout(Duration::from_millis(5000));
            let mut found = false;
            loop {
                let mut byte = [0u8; 1];
                match self.device.read_exact(&mut byte) {
                    Ok(_) => {
                        if byte[0] == 0xC0 {
                            found = true;
                            break;
                        }
                        trace!("[JUMP_DA] skip 0x{:02X}, waiting for 0xC0", byte[0]);
                    }
                    Err(e) => {
                        trace!("[JUMP_DA] read error waiting for 0xC0: {}", e);
                        break;
                    }
                }
            }
            self.device.set_timeout(orig_timeout);

            if found {
                info!("jump_da 成功: addr=0x{:08X}, attempt={}", addr, attempt);
                return Ok(true);
            }

            last_err = "jump_da: 未收到 0xC0".to_string();
            trace!("[JUMP_DA] attempt {}: 未收到 0xC0，重试", attempt);
        }
        Err(last_err)
    }

    /// JUMP_BL: 跳转到 Bootloader
    /// Python: echo(JUMP_BL) → rword() → if <=0xFF → rword() → if <=0xFF → True
    ///
    /// 注：system 重启路径已统一改用硬件看门狗（trigger_meta_reboot），jump_bl 不再被
    /// 调用；作为 BROM 直接跳 bootloader 的独立原语（不经看门狗复位）予以保留。
    #[allow(dead_code)]
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
}
