//! BROM register access (core operation for DA injection)
//!
//! - `brom_register_access` — generic 0xDA protocol (read/write)
//! - retry / clear_halt fault-tolerance logic
//!
//! Protocol: cmd(D1/DA) → mode(4B) → address(4B) → length_dwords(4B) → status(2B) → data → status(2B)
//! mode: 0=read, 1=write
//! length_dwords: number of DWORDs (the unit the device expects)

use super::core::Preloader;
use log::trace;
use std::time::Duration;

const BROM_REG_READ_TIMEOUT_MS: u64 = 10000;
const BROM_REG_RETRY_ATTEMPTS: usize = 4;
const BROM_REG_RETRY_DELAY_MS: u64 = 100;

impl Preloader {
    /// BROM register access (core operation for DA injection)
    pub fn brom_register_access(
        &mut self,
        mode: u32, // 0=read, 1=write
        address: u32,
        length_bytes: u32, // byte count, matching Python brom_register_access
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        // Command byte 0xDA: use the echo protocol (matches Python Cmd.brom_register_access.value)
        // Note: Python does not check the echo return value, so here we only log a warning instead of returning an error
        if !self.echo_1byte(0xDA)? {
            trace!("brom_reg: echo 0xDA 不匹配（继续执行）");
        }
        if !self.echo_4byte(mode)? {
            trace!("brom_reg: echo mode 不匹配（继续执行）");
        }
        if !self.echo_4byte(address)? {
            trace!("brom_reg: echo addr 不匹配（继续执行）");
        }
        if !self.echo_4byte(length_bytes)? {
            trace!("brom_reg: echo len 不匹配（继续执行）");
        }

        // Read 2 status bytes (tolerant mode, matching Python's lenient style)
        let mut st = [0u8; 2];
        if let Err(e) = self.device.read_exact(&mut st) {
            trace!("brom_reg status1 read failed: {}, continue", e);
        } else {
            trace!("brom_reg status1: {:02X?}", st);
        }

        if let Some(wdata) = data {
            // Write mode: send length_bytes of data, then read status2
            let byte_count = length_bytes as usize;
            self.device
                .write(&wdata[..byte_count])
                .map_err(|e| format!("brom_reg write data: {}", e))?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device
                    .read_exact(&mut st2)
                    .map_err(|e| format!("brom_reg status2: {}", e))?;
                trace!("brom_reg status2: {:02X?}", st2);
            }
            Ok(None)
        } else {
            // Read mode: read length_bytes bytes
            let byte_count = length_bytes as usize;
            let mut buf = vec![0u8; byte_count];

            trace!("brom_reg 准备读 {} 字节数据", byte_count);

            // Do not clear_halt before a normal read under WinUSB/nusb.
            // clear_halt issues a standard CLEAR_FEATURE control transfer that may interrupt BROM's
            // current data/status return stream; Python's success path also skips this step.
            self.device
                .set_timeout(Duration::from_millis(BROM_REG_READ_TIMEOUT_MS));

            let mut success = false;
            for attempt in 0..BROM_REG_RETRY_ATTEMPTS {
                match self.device.read_exact(&mut buf) {
                    Ok(n) if n == byte_count => {
                        trace!("brom_reg read data 成功: {} 字节", n);
                        success = true;
                        break;
                    }
                    _ => {
                        trace!("brom_reg read attempt {} failed, flush...", attempt + 1);
                        self.flush_input();
                        std::thread::sleep(Duration::from_millis(BROM_REG_RETRY_DELAY_MS));
                    }
                }
            }

            if !success {
                return Err("brom_reg read data timeout after retries".into());
            }

            // Read status2
            let mut st2 = [0u8; 2];
            let _ = self.device.read_exact(&mut st2);
            trace!("brom_reg status2: {:02X?}", st2);

            Ok(Some(buf))
        }
    }
}
