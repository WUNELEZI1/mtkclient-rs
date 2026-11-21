//! BROM IO operations: DA load/jump + ME_ID/SOC_ID queries
//!
//! - `send_da`    — send the Download Agent (dynamic EP packet size chunking + retry + ZLP)
//! - `jump_da`    — jump to DA (5 retries, each with clear_halt + flush)
//! - `jump_bl`    — jump to Bootloader
//! - `get_me_id`  — read ME_ID
//! - `get_soc_id` — read SOC_ID

use super::core::Preloader;
use log::{info, trace};
use std::time::{Duration, Instant};

const DA_UPLOAD_TIMEOUT_MS: u64 = 2000;
const DA_UPLOAD_RETRY: u32 = 5;
const DA_UPLOAD_RETRY_DELAY_MS: u64 = 50;
const JUMP_DA_MAX_ATTEMPT: u32 = 5;

impl Preloader {
    /// SEND_DA: send the Download Agent to the device
    /// Matches the Python mtkclient 2.0.1 implementation
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

        // 1. echo(0xD7) command
        // Matches Python mtk_preloader.py: echo() converts the int via pack(">I") and sends 4 bytes.
        // In Preloader serial mode the device expects a 4-byte command; 1 byte causes a timeout.
        if !self.echo_4byte(0xD7)? {
            return Err("SEND_DA: echo 0xD7 不匹配".into());
        }

        // 2. Send parameters
        if !self.echo_4byte(address)? {
            return Err("SEND_DA: echo addr 不匹配".into());
        }
        if !self.echo_4byte(size)? {
            return Err("SEND_DA: echo size 不匹配".into());
        }
        if !self.echo_4byte(sig_len)? {
            return Err("SEND_DA: echo sig_len 不匹配".into());
        }

        // 3. rword() reads status
        let status = self.rword()?;
        trace!("SEND_DA status: {:04X}", status);
        if status > 0xFF {
            return Err(format!("SEND_DA status error: {:04X}", status));
        }
        if status == 0x1D0D {
            return Err("SLA required".into());
        }

        // 4. Upload data
        let upload_start = Instant::now();
        // mtkclient 2.1.4.1 optimization: use dynamic EP_OUT.wMaxPacketSize instead of fixed 64B.
        // Keep a minimum of 64 in case the endpoint reports an abnormal value.
        let chunk_size = (self.device.out_ep_max_packet_size() as usize).max(64);
        trace!(
            "[UPLOAD] sending {} bytes, chunk={}",
            dadata.len(),
            chunk_size
        );

        // 4a. Set timeout
        let orig_timeout = self.device.get_timeout();
        self.device
            .set_timeout(Duration::from_millis(DA_UPLOAD_TIMEOUT_MS));

        // 4b. Send data (dynamic chunk, tail auto-fitted)
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

        // 4d. Match Python: 35ms delay after ZLP to let the device process data
        std::thread::sleep(Duration::from_millis(35));

        // 4e. Restore timeout
        self.device.set_timeout(orig_timeout);

        // 5. Read checksum + status
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

    /// JUMP_DA: jump to the Download Agent
    /// Protocol: echo(0xD5) → echo(addr) → write(addr) → [0x00...] → 0xC0
    /// The second addr triggers the jump; BROM may send 0~2 0x00 padding bytes, then DA sends 0xC0 to sync
    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        let mut last_err = String::new();
        for attempt in 1..=JUMP_DA_MAX_ATTEMPT {
            self.flush_input_poll(Duration::from_millis(5), 40).ok();

            if !self.echo_1byte(0xD5)? {
                last_err = "jump_da: echo 0xD5 不匹配".to_string();
                trace!("[JUMP_DA] attempt {}: echo 0xD5 不匹配，重试", attempt);
                continue;
            }
            // First addr: BROM echoes it back normally
            if !self.echo_4byte(addr)? {
                last_err = "jump_da echo addr(1): mismatch".to_string();
                trace!(
                    "[JUMP_DA] attempt {}: 第一次 echo addr 不匹配，重试",
                    attempt
                );
                self.flush_input();
                continue;
            }
            // Second addr: triggers the jump
            self.device
                .write(&addr.to_be_bytes())
                .map_err(|e| format!("jump_da write addr(2): {}", e))?;

            // Wait for the DA's 0xC0 sync byte (skip 0x00 padding bytes)
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

    /// JUMP_BL: jump to Bootloader
    /// Python: echo(JUMP_BL) → rword() → if <=0xFF → rword() → if <=0xFF → True
    ///
    /// Used by the system reboot/jump path: falls back when the watchdog reset
    /// (trigger_meta_reboot's WRITE32 flow) fails, commanding BROM to jump straight to
    /// the Bootloader so the device enters Preloader.
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
