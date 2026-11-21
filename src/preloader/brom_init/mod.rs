//! BROM protocol core: handshake, watchdog disable, sync sequence, HW code read, etc.
//!
//! Provides all primitives needed by the `Preloader` init flow:
//! - `init` full flow (handshake + HW code + watchdog off + sync)
//! - `sync_brom` sync sequence FE/FF/FC
//! - `echo_1byte` / `echo_4byte` / `sendcmd` BROM echo protocol
//! - `get_hw_code` / `get_target_config` / `get_hw_subcode`
//! - `flush_input` / `rword` / `rdword` / `rbyte` basic read helpers

use super::core::Preloader;
use crate::color::Colorize;
use crate::system::config::{CHIP_CONFIGS, TargetConfig};
use log::trace;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WATCHDOG_TIMEOUT_SECS: u64 = 3;
// USB devices respond very fast (microseconds), so a 200ms timeout is too conservative
// 50ms covers the slowest USB control transfer (serial mode overrides the timeout in the driver)
const ECHO_TIMEOUT_MS: u64 = 50;
// Normal flush: 100ms timeout, 10KB max (2x faster than before)
const FLUSH_INPUT_TIMEOUT_MS: u64 = 100;
const FLUSH_INPUT_CHUNK: usize = 1024;
const FLUSH_INPUT_MAX_ITER: usize = 10;
// Quick flush: 20ms timeout, 2KB max (2.5x faster than before)
const FLUSH_INPUT_QUICK_TIMEOUT_MS: u64 = 20;
const FLUSH_INPUT_QUICK_CHUNK: usize = 512;
const FLUSH_INPUT_QUICK_MAX_ITER: usize = 4;
const POST_FLUSH_TIMEOUT_MS: u64 = 5000;
const WDT_MAGIC: u32 = 0x22000000;

pub(crate) mod init;

impl Preloader {
    /// BROM sync sequence (FE, FF, FC)
    /// Brings the device to a ready state after a handshake or exploit
    pub fn sync_brom(&mut self) -> Result<(), String> {
        trace!("开始 BROM 同步序列...");

        // 1. BROM sync: FE is not echoed verbatim (reference log: write FE → read 0x03)
        self.device
            .write(&[0xFE])
            .map_err(|e| format!("BROM sync FE write: {}", e))?;
        let mut fe_resp = [0u8; 1];
        self.device
            .read_exact(&mut fe_resp)
            .map_err(|e| format!("BROM sync FE read: {}", e))?;
        trace!("BROM sync FE 响应: 0x{:02X}", fe_resp[0]);

        // 2. BROM FF: echo(0xFF) -> read the response
        self.device
            .write(&[0xFF])
            .map_err(|e| format!("BROM FF write: {}", e))?;
        let mut ff_resp = [0u8; 1];
        self.device
            .read_exact(&mut ff_resp)
            .map_err(|e| format!("BROM FF read: {}", e))?;
        trace!("BROM FF 响应: 0x{:02X}", ff_resp[0]);

        // 3. BROM FC: echo(0xFC) -> read 8 bytes of HW info
        if !self.echo_1byte(0xFC)? {
            return Err("BROM FC echo 不匹配".into());
        }
        let mut hw_info = [0u8; 8];
        self.device
            .read_exact(&mut hw_info)
            .map_err(|e| format!("BROM read hw_info: {}", e))?;
        trace!("BROM HW info (FC) OK: {:02X?}", hw_info);

        Ok(())
    }

    /// Full init: handshake + disable watchdog + read device info (BROM mode)
    pub fn init(&mut self) -> Result<bool, String> {
        // 1. Handshake
        if !self.device.do_handshake()? {
            return Ok(false);
        }

        // 2. Read the HW code first to match the chip config
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知芯片: HW code 0x{:04X}", hw))?;
        self.chip = Some(*chip);

        // 2.4 Read the HW Subcode: some chips combine it with hw_code to tell same-family variants apart.
        // Failure does not block init (some old chips lack 0xDB); the input buffer is then flushed to avoid stale data.
        match self.get_hw_subcode() {
            Ok(subcode) => trace!("HW subcode: {:04X}", subcode),
            Err(e) => trace!("HW subcode 读取失败（忽略）: {}", e),
        }
        self.flush_input();

        // 2.5 Spawn a background thread to preload and parse the DA file in parallel with
        // bypass / DA sending, saving the 2-4 seconds of file I/O + header parsing
        // Note: match entries inside the DA file with chip.da_code (0x6768), not hw_code (0x0707)
        let da_code_for_parse = chip.da_code;
        let da_parsed = Arc::new(Mutex::new(None));
        let da_parsed_clone = Arc::clone(&da_parsed);
        // Copy da_path into the closure up front to avoid borrowing self
        let da_path_for_thread = if !self.da_path.is_empty() {
            self.da_path.clone()
        } else {
            String::new()
        };
        std::thread::spawn(move || {
            use crate::da::loader::header::parse_da_header;
            use crate::system::paths::get_exe_relative_path;
            use std::fs::File;
            use std::io::Read;

            let da_path = if !da_path_for_thread.is_empty() {
                std::path::PathBuf::from(&da_path_for_thread)
            } else {
                get_exe_relative_path("MTK_DA_V5.bin")
            };
            let mut file = match File::open(&da_path) {
                Ok(f) => f,
                Err(_) => return,
            };
            let mut da_data = Vec::new();
            if file.read_to_end(&mut da_data).is_err() {
                return;
            }
            match parse_da_header(&da_data, da_code_for_parse) {
                Ok((_magic, regions, _is_v6)) => {
                    if let Ok(mut guard) = da_parsed_clone.lock() {
                        *guard = Some((da_data, regions));
                    }
                }
                Err(e) => {
                    trace!("[DA_PRELOAD] 后台解析 DA header 失败: {}", e);
                }
            }
        });
        self.da_parsed = Some(da_parsed);

        // 3. Disable the watchdog
        self.disable_watchdog()?;

        // 4. get_target_config — matches reference log lines 101-108
        let _ = self.get_target_config();

        // 5-7. BROM sync (FE, FF, FC)
        self.sync_brom()?;

        trace!("BROM 模式初始化成功");
        self.brom_initialized = true;
        Ok(true)
    }

    /// Disable the watchdog (needed in both Preloader and BROM mode)
    ///
    /// Writes WDT_MAGIC (0x22000000) to the watchdog register via the BROM write32 protocol.
    /// In Preloader mode the device reboots after about 3 seconds unless the watchdog is disabled.
    fn disable_watchdog(&mut self) -> Result<(), String> {
        let chip = self
            .chip
            .as_ref()
            .ok_or("disable_watchdog: chip 未初始化")?;

        trace!("[WD] 开始关闭看门狗流程 (write32协议)");
        self.device
            .set_timeout(Duration::from_secs(WATCHDOG_TIMEOUT_SECS));

        let wdt_addr = chip.watchdog;
        let wdt_value: u32 = WDT_MAGIC;

        trace!("[WD] 地址=0x{:08X}, value=0x{:08X}", wdt_addr, wdt_value);

        trace!("[WD] 步骤1: echo_1byte(0xD4)");
        if !self.echo_1byte(0xD4)? {
            return Err("关闭看门狗: echo 0xD4 不匹配".into());
        }
        trace!("[WD] 步骤2: echo_4byte(addr=0x{:08X})", wdt_addr);
        if !self.echo_4byte(wdt_addr)? {
            return Err("关闭看门狗: echo addr 不匹配".into());
        }
        trace!("[WD] 步骤3-4: echo_4byte(count=1) 并读取 status1");
        let status1 = self.echo_4byte_then_status(1)?;
        trace!("[WD] status1: 0x{:04X}", status1);
        if status1 > 0xFF {
            return Err(format!("关闭看门狗失败: status1=0x{:04X}", status1));
        }
        trace!(
            "[WD] 步骤5-6: echo_4byte(value=0x{:08X}) 并读取 status2",
            wdt_value
        );
        let status2 = self.echo_4byte_then_status(wdt_value)?;
        trace!("[WD] status2: 0x{:04X}", status2);
        if status2 > 0xFF {
            return Err(format!("关闭看门狗失败: status2=0x{:04X}", status2));
        }

        trace!("{}", "看门狗已关闭".green().bold());
        Ok(())
    }

    /// Preloader mode init: handshake + read HW code + disable watchdog
    pub fn init_preloader(&mut self) -> Result<bool, String> {
        // 1. Handshake
        if !self.device.do_handshake()? {
            return Ok(false);
        }

        // 2. Read the HW code to match the chip config
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知芯片: HW code 0x{:04X}", hw))?;
        self.chip = Some(*chip);

        // 3. Disable the watchdog (needed in Preloader mode too, or the device reboots after 3 seconds)
        self.disable_watchdog()?;

        trace!("Preloader 模式初始化成功, chip={}", chip.name);
        self.brom_initialized = true;
        self.is_preloader_mode = true;
        Ok(true)
    }

    /// Trigger a watchdog reboot (used before the Preloader Pattern protocol)
    ///
    /// In serial mode the device does not support brom_register_access(0xDA) (echo works but no status),
    /// so the watchdog WDT_RESTART register is written directly via BROM WRITE32.
    ///
    /// After the reboot Preloader signals READY; the Pattern protocol then sends FASTBOOT immediately.
    ///
    /// If any watchdog WRITE32 step fails, fall back to `jump_bl` (BROM 0xD6) to command BROM
    /// to jump to the Bootloader, matching the "leave BROM into Preloader" effect of a watchdog reset.
    pub fn trigger_meta_reboot(&mut self) -> Result<(), String> {
        match self.reboot_via_watchdog() {
            Ok(()) => Ok(()),
            Err(e) => {
                trace!("[META] 看门狗重启失败: {}，回退到 jump_bl(0xD6)", e);
                if self.jump_bl()? {
                    Ok(())
                } else {
                    Err("trigger_meta_reboot: 看门狗重启与 jump_bl 均失败".to_string())
                }
            }
        }
    }

    /// Trigger a reboot by writing the watchdog WDT_RESTART register via BROM WRITE32
    fn reboot_via_watchdog(&mut self) -> Result<(), String> {
        let chip = self
            .chip
            .as_ref()
            .ok_or("trigger_meta_reboot: chip 未初始化")?;

        let reboot_addr = chip.watchdog + 0x14;
        let reboot_value: u32 = 0x00001209;

        // Trigger the watchdog reboot with a direct write32 (skips brom_register_access; serial unsupported)
        trace!(
            "[META] write32(0x{:08X}, 0x{:08X}) 触发看门狗重启",
            reboot_addr, reboot_value
        );

        // BROM WRITE32 protocol: echo_1byte(0xD4) → echo_4byte(addr) → echo_4byte(count) → echo_4byte(value) → rword()
        trace!("[META] write32: echo_1byte(0xD4)");
        if !self.echo_1byte(0xD4)? {
            return Err("meta_reset: echo 0xD4 不匹配".into());
        }
        trace!("[META] write32: echo_4byte(addr=0x{:08X})", reboot_addr);
        if !self.echo_4byte(reboot_addr)? {
            return Err("meta_reset: echo addr 不匹配".into());
        }
        trace!("[META] write32: echo_4byte(count=1)");
        if !self.echo_4byte(1)? {
            return Err("meta_reset: echo count 不匹配".into());
        }
        trace!("[META] write32: echo_4byte(value=0x{:08X})", reboot_value);
        if !self.echo_4byte(reboot_value)? {
            return Err("meta_reset: echo value 不匹配".into());
        }
        trace!("[META] write32: rword() 读 status");
        let status = self.rword()?;
        trace!("[META] write32 status: 0x{:04X}", status);
        if status > 0xFF {
            trace!(
                "[META] write32 status 异常 (0x{:04X})，但设备可能已重启",
                status
            );
        }

        trace!("{}", "看门狗重启已触发".green().bold());
        Ok(())
    }

    /// BROM echo protocol: fully matches Python Port.echo() (Port.py:210-229)
    /// After a DA upload the device may emit debug output, leaving residual data on echo reads.
    /// Up to 32 residual bytes are tolerated; reading continues until the real echo shows up.
    pub fn echo_1byte(&mut self, cmd: u8) -> Result<bool, String> {
        let orig_timeout = self.device.get_timeout();
        self.device
            .set_timeout(Duration::from_millis(ECHO_TIMEOUT_MS));
        let result = self.echo_1byte_impl(cmd);
        self.device.set_timeout(orig_timeout);
        result
    }

    fn echo_1byte_impl(&mut self, cmd: u8) -> Result<bool, String> {
        if let Err(e) = self.device.write(&[cmd]) {
            // PIPE error (endpoint STALL): clear_halt then retry once
            if e.contains("err -7") {
                trace!("[ECHO_1] write PIPE error, clear_halt_out + retry...");
                let _ = self.device.clear_halt_out();
                std::thread::sleep(Duration::from_millis(50));
                self.device
                    .write(&[cmd])
                    .map_err(|e2| format!("echo write: {}", e2))?;
            } else {
                return Err(format!("echo write: {}", e));
            }
        }
        let mut buf = [0u8; 1];
        for i in 0..8 {
            match self.device.read_exact(&mut buf) {
                Ok(_) => {
                    if buf[0] == cmd {
                        if i > 0 {
                            trace!(
                                "[ECHO_1] skipped {} residual bytes, echo 0x{:02X} matched",
                                i, cmd
                            );
                        }
                        return Ok(true);
                    } else {
                        trace!(
                            "[ECHO_1] skip residual 0x{:02X} (attempt {}), waiting for 0x{:02X}",
                            buf[0],
                            i + 1,
                            cmd
                        );
                    }
                }
                Err(e) => {
                    trace!(
                        "[ECHO_1] read error for 0x{:02X} after {} attempts: {}",
                        cmd, i, e
                    );
                    return Ok(false);
                }
            }
        }
        trace!(
            "[ECHO_1] mismatch: expected 0x{:02X}, too much residual data after 8 reads",
            cmd
        );
        self.flush_input_quick();
        Ok(false)
    }

    /// Send a 4-byte big-endian value and verify the echo (matches Python pack(">I", val))
    /// Tolerates residual data: reads byte by byte through a 4-byte sliding-window ring buffer
    /// and skips residuals to match the expected echo (same mechanism as echo_1byte).
    pub fn echo_4byte(&mut self, val: u32) -> Result<bool, String> {
        let be = val.to_be_bytes();
        trace!("[ECHO_4] 发送: {:02X?} (值=0x{:08X})", be, val);
        if let Err(e) = self.device.write(&be) {
            if e.contains("err -7") {
                trace!("[ECHO_4] write PIPE error, clear_halt_out + retry...");
                let _ = self.device.clear_halt_out();
                std::thread::sleep(Duration::from_millis(50));
                self.device
                    .write(&be)
                    .map_err(|e2| format!("echo_4byte write: {}", e2))?;
            } else {
                return Err(format!("echo_4byte write: {}", e));
            }
        }

        // Read byte by byte, maintaining the 4-byte sliding-window ring buffer
        let mut ring = [0u8; 4];
        let mut ring_pos = 0usize;
        let mut ring_filled = 0usize;
        let mut buf = [0u8; 1];

        for i in 0..8 {
            match self.device.read_exact(&mut buf) {
                Ok(_) => {
                    ring[ring_pos] = buf[0];
                    ring_pos = (ring_pos + 1) % 4;
                    if ring_filled < 4 {
                        ring_filled += 1;
                    }

                    if ring_filled >= 4 {
                        // Reassemble the ring buffer into 4 contiguous bytes in write order
                        let window = [
                            ring[(ring_pos) % 4],
                            ring[(ring_pos + 1) % 4],
                            ring[(ring_pos + 2) % 4],
                            ring[(ring_pos + 3) % 4],
                        ];
                        if window == be {
                            let skipped = i + 1 - 4;
                            if skipped > 0 {
                                trace!(
                                    "[ECHO_4] 跳过 {} 字节残余数据，回显 {:02X?} 匹配",
                                    skipped, be
                                );
                            }
                            return Ok(true);
                        } else if i < 3 {
                            // Still filling the ring buffer; log the residual byte
                            trace!(
                                "[ECHO_4] 跳过残余字节 0x{:02X} (第 {} 次)，等待 {:02X?}",
                                buf[0],
                                i + 1,
                                be
                            );
                        } else {
                            trace!(
                                "[ECHO_4] 窗口 {:02X?} 不匹配 {:02X?}，继续读取...",
                                window, be
                            );
                        }
                    } else {
                        trace!(
                            "[ECHO_4] 填充环缓冲区：接收 0x{:02X} (已填充 {}/4)",
                            buf[0], ring_filled
                        );
                    }
                }
                Err(e) => {
                    trace!("[ECHO_4] 读取 0x{:08X} 超时/错误 (第 {} 次): {}", val, i, e);
                    return Ok(false);
                }
            }
        }

        trace!("[ECHO_4] 8 次读取后仍未匹配 {:02X?}，残余数据过多", be);
        self.flush_input_quick();
        Ok(false)
    }

    /// Send a 4-byte big-endian value, verify the echo, then read the 2-byte status.
    /// Matches the reference watchdog flow: write 4B -> read 4B echo -> read 2B status.
    pub fn echo_4byte_then_status(&mut self, val: u32) -> Result<u16, String> {
        if !self.echo_4byte(val)? {
            return Err(format!("4-byte echo mismatch: 0x{:08X}", val));
        }
        let status = self.rword()?;
        trace!("4-byte status for {:08X}: {:04X}", val, status);
        Ok(status)
    }

    /// Send a 1-byte command (equivalent to Python echo(Cmd.XXX.value))
    pub fn sendcmd(&mut self, cmd: u8) -> Result<bool, String> {
        self.echo_1byte(cmd)
    }

    /// Read the hardware code
    /// Python: echo(Cmd.GET_HW_CODE.value) → rbyte(4) → unpack(">HH")
    pub fn get_hw_code(&mut self) -> Result<u16, String> {
        if !self.sendcmd(0xFD)? {
            return Err("获取 HW code 失败: echo 0xFD 不匹配".into());
        }
        let mut buf = [0u8; 4];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read hwcode: {}", e))?;
        let hw = u16::from_be_bytes([buf[0], buf[1]]);
        trace!("HW code: {:04X}", hw);
        Ok(hw)
    }

    /// Read the target device security config
    /// Python mtk_preloader.py:517-542: echo(0xD8) → rbyte(6) → unpack(">IH")
    pub fn get_target_config(&mut self) -> Result<TargetConfig, String> {
        if !self.sendcmd(0xD8)? {
            return Err("获取 target config 失败: echo 0xD8 不匹配".into());
        }

        let mut buf = [0u8; 6];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read target config: {}", e))?;
        let target_config = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let status = u16::from_be_bytes([buf[4], buf[5]]);
        trace!(
            "Target config: {:08X}, status: {:04X}",
            target_config, status
        );
        if status > 0xFF {
            return Err(format!("Get Target Config Error: status=0x{:04X}", status));
        }

        // chip is already set in init() step 2, so do not call it again
        Ok(TargetConfig::from_raw(target_config))
    }
}
