//! XFlash 协议环境初始化
//!
//! - `setup_env`     — CMD_SETUP_ENVIRONMENT
//! - `setup_hw_init` — CMD_SETUP_HW_INIT_PARAMS
//! - `send_emi`      — CMD_INIT_EXT_RAM + EMI 数据
//! - `boot_to`       — CMD_BOOT_TO + 发送 DA Stage2

use log::{info, trace, warn};
use std::thread::sleep;
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{
    CMD_BOOT_TO, CMD_INIT_EXT_RAM, CMD_MAGIC, CMD_SETUP_ENVIRONMENT, CMD_SETUP_HW_INIT_PARAMS,
    CMD_SYNC_SIGNAL, pack3,
};

fn boot_to_should_clear_halt_before_write() -> bool {
    false
}

impl<'a> DAXFlash<'a> {
    /// 带重试的 USB 写入：第2次起尝试 clear_halt_out 恢复 stalled endpoint
    pub(crate) fn write_with_retry(&mut self, data: &[u8], label: &str) -> Result<(), String> {
        const MAX_RETRY: u32 = 5;
        const RETRY_DELAY_MS: u64 = 100;
        for attempt in 1..=MAX_RETRY {
            if crate::cancel::force_requested() || crate::cancel::requested() {
                self.preloader.device.cancel_pending_transfers();
                return Err(format!("{} write 已取消", label));
            }
            match self.preloader.device.write(data) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    trace!(
                        "[RETRY] {} write fail (attempt {}/{}): {}",
                        label, attempt, MAX_RETRY, e
                    );
                    if attempt >= 2 {
                        // 第2次起尝试 clear_halt_out 恢复 stalled endpoint
                        trace!("[RETRY] clear_halt_out for {}", label);
                        let _ = self.preloader.device.clear_halt_out();
                    }
                    if attempt < MAX_RETRY {
                        sleep(Duration::from_millis(RETRY_DELAY_MS));
                    } else {
                        return Err(format!(
                            "{} write 失败 ({}次重试后): {}",
                            label, MAX_RETRY, e
                        ));
                    }
                }
            }
        }
        unreachable!()
    }

    /// 设置环境
    /// Python: xsend(CMD_SETUP_ENVIRONMENT) → send_param(20字节) → status()
    pub fn setup_env(&mut self) -> Result<bool, String> {
        trace!("设置环境...");

        // xsend(CMD_SETUP_ENVIRONMENT)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "setup_env xsend")?;
        self.write_with_retry(&CMD_SETUP_ENVIRONMENT.to_le_bytes(), "setup_env CMD")?;

        // send_param(20字节): da_log_level, log_channel, system_os, ufs_provision, 0x0
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

    /// 初始化硬件
    /// Python: xsend(CMD_SETUP_HW_INIT_PARAMS) → send_param(pack("<I", 0x0)) → status()
    pub fn setup_hw_init(&mut self) -> Result<bool, String> {
        info!("初始化硬件...");

        // xsend(CMD_SETUP_HW_INIT_PARAMS)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.write_with_retry(&pkt, "setup_hw_init xsend")?;
        self.write_with_retry(&CMD_SETUP_HW_INIT_PARAMS.to_le_bytes(), "setup_hw_init CMD")?;

        // send_param(pack("<I", 0x0)): 4字节参数 = 0x0
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

    /// 发送 EMI 数据初始化 DRAM
    /// 流程：发送 INIT_EXT_RAM → 发送 EMI 数据 → 验证状态
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
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_INIT_EXT_RAM.to_le_bytes())?;

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
        self.preloader.device.write(&pkt2)?;
        self.preloader
            .device
            .write(&(emi.len() as u32).to_le_bytes())?;

        // 5. send_param([emi]) - Python sends param header + data in 512-byte chunks
        let param_pkt = pack3(CMD_MAGIC, 0x01, emi.len() as u32);
        self.preloader.device.write(&param_pkt)?;

        // Python send_param splits data into 0x200 (512) byte chunks
        let chunk_size = 0x200;
        let mut pos = 0;
        let mut remaining = emi.len();
        while remaining > 0 {
            let dsize = std::cmp::min(remaining, chunk_size);
            self.preloader.device.write(&emi[pos..pos + dsize])?;
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

    /// Boot 到指定地址
    /// 对齐 Python boot_to + send_data 流程
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

        // WinUSB/nusb 下不要在正常 boot_to 前 clear_halt。
        // clear_halt 会发 CLEAR_FEATURE control transfer，Python 成功路径没有这一步。
        if boot_to_should_clear_halt_before_write() && self.preloader.device.is_libusb() {
            let _ = self.preloader.device.clear_halt_in();
            let _ = self.preloader.device.clear_halt_out();
        }

        // 设置足够的写入超时
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

        // Python: self.send_data(da) — 发送 12 字节头 + 大块数据
        // USB bulk 传输支持任意大小的 buffer，底层驱动自动拆分
        // 使用 64KB 分块（而非 EP maxPacketSize 512B），大幅减少系统调用次数
        let pkt2 = pack3(CMD_MAGIC, 0x01, da.len() as u32);
        self.write_with_retry(&pkt2, "boot_to data header")?;

        const BULK_CHUNK: usize = 0x10000; // 64KB — USB bulk 最佳性能分块
        let mut remaining = da.len();
        let mut pos = 0;

        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, BULK_CHUNK);
            let chunk = &da[pos..pos + chunk_size];

            match self.preloader.device.write(chunk) {
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

        // 恢复超时
        self.preloader.device.set_timeout(orig_timeout);

        // 对齐 Python boot_to：先 sleep 让设备执行跳转代码，然后只读一次 status。
        // 不能用轮询——轮询会消耗 SYNC 包，导致后续 DA extensions 的 DEVICE_CTRL
        // 读到错误状态（SYNC 被提前消费），DA extensions 无法加载。
        let sleep_ms = (timeout * 1000.0) as u64;
        if sleep_ms > 0 {
            sleep(Duration::from_millis(sleep_ms));
        }

        // Python: status = self.status() — 只读一次
        // 接受 SYNC (0x434E5953) 或 0x0 作为成功
        match self.status() {
            Ok(st2) => {
                if st2 == CMD_SYNC_SIGNAL || st2 == 0x0 {
                    if display {
                        trace!("Boot 成功 (status=0x{:08X})", st2);
                    }
                    Ok(true)
                } else {
                    // Python: 其他状态码只打印 error，不抛异常
                    if display {
                        warn!("boot_to 状态: 0x{:08X}", st2);
                    }
                    Ok(true) // 不返回错误，继续执行
                }
            }
            Err(_) => {
                // Python: status 读取失败时打印 error 但继续
                if display {
                    warn!("boot_to status 读取失败（设备已重新枚举）");
                }
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_to_does_not_clear_halt_before_normal_write_flow() {
        assert!(!boot_to_should_clear_halt_before_write());
    }
}
