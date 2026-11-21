//! XFlash protocol environment initialization
//!
//! - `setup_env`     — CMD_SETUP_ENVIRONMENT
//! - `setup_hw_init` — CMD_SETUP_HW_INIT_PARAMS
//! - `send_emi`      — CMD_INIT_EXT_RAM + EMI data
//! - `boot_to`       — CMD_BOOT_TO + send DA Stage2

use log::{info, trace, warn};
use std::thread::sleep;
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{
    CMD_BOOT_TO, CMD_INIT_EXT_RAM, CMD_MAGIC, CMD_SETUP_ENVIRONMENT, CMD_SETUP_HW_INIT_PARAMS,
    CMD_SYNC_SIGNAL, pack3,
};

impl<'a> DAXFlash<'a> {
    /// USB write with retries: from the second attempt on, wait first and then retry plainly.
    ///
    /// Note: under WinUSB/nusb, do **not** actively clear_halt_out to "recover" a stalled endpoint.
    /// On older DA platforms (MT6771/0x788 etc.) endpoint stalls during setup_env are handled by DA flow
    /// control; issuing a CLEAR_FEATURE control transfer makes WinUSB return ERROR_GEN_FAILURE(31),
    /// turning a retryable write failure into an irreversible os error 31 (matching the brom_reg_access.rs convention).
    /// So clear_halt is only a last resort for the libusb backend after all plain retries failed, and its error is ignored.
    pub(crate) fn write_with_retry(&mut self, data: &[u8], label: &str) -> Result<(), String> {
        const MAX_RETRY: u32 = 5;
        const RETRY_DELAY_MS: u64 = 100;
        let mut last_err;
        for attempt in 1..=MAX_RETRY {
            if crate::cancel::force_requested() || crate::cancel::requested() {
                self.preloader.device.cancel_pending_transfers();
                return Err(format!("{} write 已取消", label));
            }
            match self.preloader.device.write(data) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_err = e.to_string();
                    trace!(
                        "[RETRY] {} write fail (attempt {}/{}): {}",
                        label, attempt, MAX_RETRY, last_err
                    );
                    if attempt < MAX_RETRY {
                        // Wait first so DA flow control can recover the endpoint
                        sleep(Duration::from_millis(RETRY_DELAY_MS));
                        // Recovery (matching the flasher unstick flow): drain leftover IN data first.
                        // If DA flow control back-pressures because the host did not read the previous
                        // command's response, blocking the ACK for the new OUT, draining IN restores it
                        // (draining an empty IN is harmless). This occurs occasionally after DA load on the
                        // brom/WinUSB backend; serial has no such concept.
                        self.preloader.device.drain_pending();
                        if attempt == MAX_RETRY - 1 {
                            // Last resort: clear OUT/IN halt (final step only; errors swallowed, not propagated).
                            // Old DA (e.g. MT6771) over WinUSB occasionally returns error 31 from clear_halt
                            // during setup_env, so trigger it only at the last step to avoid worsening things.
                            let _ = self.preloader.device.clear_halt_out();
                            let _ = self.preloader.device.clear_halt_in();
                        }
                    } else {
                        return Err(format!(
                            "{} write 失败 ({}次重试后): {}",
                            label, MAX_RETRY, last_err
                        ));
                    }
                }
            }
        }
        unreachable!()
    }

    /// Set up the environment
    /// Python: xsend(CMD_SETUP_ENVIRONMENT) → send_param(20 bytes) → status()
    pub fn setup_env(&mut self) -> Result<bool, String> {
        trace!("设置环境...");

        // xsend(CMD_SETUP_ENVIRONMENT)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "setup_env xsend")?;
        self.write_with_retry(&CMD_SETUP_ENVIRONMENT.to_le_bytes(), "setup_env CMD")?;

        // send_param(20 bytes): da_log_level, log_channel, system_os, ufs_provision, 0x0
        let param: [u8; 20] = [
            0x00, 0x00, 0x00, 0x00, // da_log_level = 0
            0x01, 0x00, 0x00, 0x00, // log_channel = 1
            0x01, 0x00, 0x00, 0x00, // system_os = OS_LINUX = 1
            0x00, 0x00, 0x00, 0x00, // ufs_provision = 0
            0x00, 0x00, 0x00, 0x00, // 0x0
        ];
        let param_pkt = pack3(CMD_MAGIC, 0x01, 20);
        self.write_with_retry(&param_pkt, "setup_env param_hdr")?;
        self.write_with_retry(&param, "setup_env param")?;

        // status
        let st = self.status()?;
        if st != 0 && st != 0xC0010003 {
            return Err(format!("setup_env status error: 0x{:08X}", st));
        }
        if st == 0xC0010003 {
            trace!("setup_env: 0xC0010003（已初始化），跳过");
        }

        trace!("环境设置成功");
        Ok(true)
    }

    /// Initialize hardware
    /// Python: xsend(CMD_SETUP_HW_INIT_PARAMS) → send_param(pack("<I", 0x0)) → status()
    pub fn setup_hw_init(&mut self) -> Result<bool, String> {
        info!("初始化硬件...");

        // xsend(CMD_SETUP_HW_INIT_PARAMS)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "setup_hw_init xsend")?;
        self.write_with_retry(&CMD_SETUP_HW_INIT_PARAMS.to_le_bytes(), "setup_hw_init CMD")?;

        // send_param(pack("<I", 0x0)): 4-byte parameter = 0x0
        let param = 0x0u32.to_le_bytes();
        let param_pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&param_pkt, "setup_hw_init param_hdr")?;
        self.write_with_retry(&param, "setup_hw_init param")?;

        // status
        let st = self.status()?;
        if st != 0 && st != 0xC0010003 {
            return Err(format!("setup_hw_init status error: 0x{:08X}", st));
        }
        if st == 0xC0010003 {
            trace!("setup_hw_init: 0xC0010003（已初始化），跳过");
        }

        trace!("硬件初始化成功");
        Ok(true)
    }

    /// Send EMI data to initialize DRAM
    /// Flow: send INIT_EXT_RAM → send EMI data → verify status
    pub fn send_emi(&mut self, emi: &[u8]) -> Result<bool, String> {
        trace!("发送 EMI 数据初始化 DRAM...");
        trace!(
            "[EMI DEBUG] len={}, first 64 bytes={}",
            emi.len(),
            emi[..std::cmp::min(64, emi.len())]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        // 1. xsend(INIT_EXT_RAM)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "send_emi xsend INIT_EXT_RAM")?;
        self.write_with_retry(&CMD_INIT_EXT_RAM.to_le_bytes(), "send_emi CMD_INIT_EXT_RAM")?;

        // 2. status() - Python reads immediately, no sleep
        let st = self.status()?;
        trace!("[EMI DEBUG] INIT_EXT_RAM status=0x{:08X}", st);
        if st != 0 {
            return Err(format!("INIT_EXT_RAM status error: 0x{:08X}", st));
        }

        // 3. sleep(0.005) - Python sleeps AFTER status check
        sleep(Duration::from_millis(5));

        // 4. xsend(len(emi)) - Python sends header + length value
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt2, "send_emi xsend len")?;
        self.write_with_retry(&(emi.len() as u32).to_le_bytes(), "send_emi emi len")?;

        // 5. send_param([emi]) - Python sends param header + data in 512-byte chunks
        let param_pkt = pack3(CMD_MAGIC, 0x01, emi.len() as u32);
        self.write_with_retry(&param_pkt, "send_emi param header")?;

        // Python send_param splits data into 0x200 (512) byte chunks
        let chunk_size = 0x200;
        let mut pos = 0;
        let mut remaining = emi.len();
        while remaining > 0 {
            let dsize = std::cmp::min(remaining, chunk_size);
            self.write_with_retry(&emi[pos..pos + dsize], "send_emi param data")?;
            pos += dsize;
            remaining -= dsize;
        }

        // 6. status() - wait for EMI config complete (Python takes ~1.1s)
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        let st3 = self.status()?;
        self.preloader.device.set_timeout(orig_timeout);

        if st3 != 0 {
            return Err(format!("EMI data status error: 0x{:08X}", st3));
        }

        trace!("EMI 数据发送成功");
        Ok(true)
    }

    /// Boot to the given address
    /// Matching the Python boot_to + send_data flow
    pub(crate) fn boot_to(
        &mut self,
        addr: u32,
        da: &[u8],
        display: bool,
        timeout: f32,
    ) -> Result<bool, String> {
        if display {
            trace!("Boot 到地址: 0x{:08X}, 大小: {} 字节", addr, da.len());
        }

        // Set a generous write timeout
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        // Python: self.xsend(self.Cmd.BOOT_TO)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "boot_to xsend BOOT_TO")?;
        self.write_with_retry(&CMD_BOOT_TO.to_le_bytes(), "boot_to CMD_BOOT_TO")?;

        // Python: self.status()
        let st = self.status()?;
        trace!("[BOOT_TO DEBUG] status1=0x{:08X} (after xsend BOOT_TO)", st);
        if st != 0 {
            return Err(format!("boot_to status1 error: 0x{:08X}", st));
        }

        // Python: param = pack("<QQ", addr, len(da)) + usbwrite(pkt1) + usbwrite(param)
        let param_len: u64 = da.len() as u64;
        let mut param = Vec::with_capacity(16);
        param.extend_from_slice(&(addr as u64).to_le_bytes());
        param.extend_from_slice(&param_len.to_le_bytes());
        let pkt1 = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.write_with_retry(&pkt1, "boot_to param header")?;
        self.write_with_retry(&param, "boot_to param data")?;

        // Python: self.send_data(da) — send 12-byte header + bulk data
        // USB bulk transfers accept buffers of any size; the driver splits them automatically
        // Use 64KB chunks (instead of EP maxPacketSize 512B) to greatly reduce syscall count
        let pkt2 = pack3(CMD_MAGIC, 0x01, da.len() as u32);
        self.write_with_retry(&pkt2, "boot_to data header")?;

        const BULK_CHUNK: usize = 0x200000; // 2MB — matches the chunk size measured with the flasher, greatly reducing syscalls and round trips
        let mut remaining = da.len();
        let mut pos = 0;

        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, BULK_CHUNK);
            let chunk = &da[pos..pos + chunk_size];

            match self.write_with_retry(chunk, "boot_to Stage2 chunk") {
                Ok(_) => {
                    pos += chunk_size;
                    remaining -= chunk_size;
                }
                Err(e) => {
                    if display {
                        warn!(
                            "Stage2 数据传输中断（设备开始执行，剩余 {} 字节）: {}",
                            remaining, e
                        );
                    }
                    break;
                }
            }
        }

        // Restore the timeout
        self.preloader.device.set_timeout(orig_timeout);

        // Matching Python boot_to: sleep first so the device can run the jump code, then read status only once.
        // Do not poll — polling consumes the SYNC packet, so later DA extensions' DEVICE_CTRL reads a wrong
        // state (SYNC consumed early) and DA extensions fail to load.
        let sleep_ms = (timeout * 1000.0) as u64;
        if sleep_ms > 0 {
            sleep(Duration::from_millis(sleep_ms));
        }

        // Python: status = self.status() — read only once
        // Accept SYNC (0x434E5953) or 0x0 as success
        match self.status() {
            Ok(st2) => {
                if st2 == CMD_SYNC_SIGNAL || st2 == 0x0 {
                    if display {
                        trace!("Boot 成功 (status=0x{:08X})", st2);
                    }
                    Ok(true)
                } else {
                    // Python: other status codes are only logged as errors, no exception
                    if display {
                        warn!("boot_to 状态: 0x{:08X}", st2);
                    }
                    Ok(true) // do not return an error; continue
                }
            }
            Err(_) => {
                // Python: log an error but continue if status read fails
                if display {
                    warn!("boot_to status 读取失败（设备已重新枚举）");
                }
                Ok(true)
            }
        }
    }
}
