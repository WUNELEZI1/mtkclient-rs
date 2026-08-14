//! BROM 协议核心：握手、关看门狗、同步序列、HW code 读取等
//!
//! 包含 `Preloader` 初始化流程中需要的所有原语：
//! - `init` 完整流程（握手 + HW code + 关看门狗 + sync）
//! - `sync_brom` 同步序列 FE/FF/FC
//! - `echo_1byte` / `echo_4byte` / `sendcmd` BROM echo 协议
//! - `get_hw_code` / `get_target_config` / `get_hw_subcode`
//! - `flush_input` / `rword` / `rdword` / `rbyte` 基础读工具

use super::core::Preloader;
use crate::system::config::{CHIP_CONFIGS, TargetConfig};
use colored::Colorize;
use log::trace;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WATCHDOG_TIMEOUT_SECS: u64 = 3;
// USB 设备响应极快（微秒级），200ms 超时过于保守
// 50ms 足以覆盖最慢的 USB 控制传输（串口模式下实际超时会被设备驱动覆盖）
const ECHO_TIMEOUT_MS: u64 = 50;
// 常规 flush: 100ms 超时，最大 10KB（比原来快 2 倍）
const FLUSH_INPUT_TIMEOUT_MS: u64 = 100;
const FLUSH_INPUT_CHUNK: usize = 1024;
const FLUSH_INPUT_MAX_ITER: usize = 10;
// 快速 flush: 20ms 超时，最大 2KB（比原来快 2.5 倍）
const FLUSH_INPUT_QUICK_TIMEOUT_MS: u64 = 20;
const FLUSH_INPUT_QUICK_CHUNK: usize = 512;
const FLUSH_INPUT_QUICK_MAX_ITER: usize = 4;
const POST_FLUSH_TIMEOUT_MS: u64 = 5000;
const WDT_MAGIC: u32 = 0x22000000;

impl Preloader {

    /// BROM 同步序列 (FE, FF, FC)
    /// 用于在握手或漏洞利用后让设备进入就绪状态
    pub fn sync_brom(&mut self) -> Result<(), String> {
        trace!("开始 BROM 同步序列...");

        // 1. BROM sync: FE 不做同字节回显（刷机匣日志：写 FE → 读 0x03）
        self.device
            .write(&[0xFE])
            .map_err(|e| format!("BROM sync FE write: {}", e))?;
        let mut fe_resp = [0u8; 1];
        self.device
            .read_exact(&mut fe_resp)
            .map_err(|e| format!("BROM sync FE read: {}", e))?;
        trace!("BROM sync FE 响应: 0x{:02X}", fe_resp[0]);

        // 2. BROM FF: echo(0xFF) -> 读响应
        self.device
            .write(&[0xFF])
            .map_err(|e| format!("BROM FF write: {}", e))?;
        let mut ff_resp = [0u8; 1];
        self.device
            .read_exact(&mut ff_resp)
            .map_err(|e| format!("BROM FF read: {}", e))?;
        trace!("BROM FF 响应: 0x{:02X}", ff_resp[0]);

        // 3. BROM FC: echo(0xFC) -> 读 8 字节 HW info
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

    /// 完整初始化：握手 + 关闭看门狗 + 读取设备信息（BROM 模式）
    pub fn init(&mut self) -> Result<bool, String> {
        // 1. 握手
        if !self.device.do_handshake()? {
            return Ok(false);
        }

        // 2. 先获取 HW code 来匹配芯片配置
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知芯片: HW code 0x{:04X}", hw))?;
        self.chip = Some(*chip);

        // 2.5 启动后台线程预读取并解析 DA 文件：bypass / 发送 DA 期间并行完成，
        // 节省文件 I/O + header 解析的 2~4 秒
        // 注意：使用 chip.da_code（0x6768）而非 hw_code（0x0707）来匹配 DA 文件内条目
        let da_code_for_parse = chip.da_code;
        let da_parsed = Arc::new(Mutex::new(None));
        let da_parsed_clone = Arc::clone(&da_parsed);
        // 提前复制 da_path 到闭包中，避免借用 self
        let da_path_for_thread = if !self.da_path.is_empty() {
            self.da_path.clone()
        } else {
            String::new()
        };
        std::thread::spawn(move || {
            use crate::da::loader::header::parse_da_header;
            use crate::system::paths::获取可执行文件相对路径;
            use std::fs::File;
            use std::io::Read;

            let da_path = if !da_path_for_thread.is_empty() {
                std::path::PathBuf::from(&da_path_for_thread)
            } else {
                获取可执行文件相对路径("MTK_DA_V5.bin")
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

        // 3. 关闭看门狗
        self.disable_watchdog()?;

        // 4. get_target_config — 对齐刷机匣第 101-108 行
        let _ = self.get_target_config();

        // 5-7. BROM sync (FE, FF, FC)
        self.sync_brom()?;

        trace!("BROM 模式初始化成功");
        self.brom_initialized = true;
        Ok(true)
    }

    /// 关闭看门狗（Preloader 和 BROM 模式都需要）
    ///
    /// 使用 BROM write32 协议将 watchdog 寄存器写入 WDT_MAGIC (0x22000000)。
    /// Preloader 模式下如果不关看门狗，设备会在约 3 秒后自动重启。
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
        trace!("[WD] 步骤3: echo_4byte(count=1)");
        if !self.echo_4byte(1)? {
            return Err("关闭看门狗: echo count 不匹配".into());
        }
        trace!("[WD] 步骤4: rword() 读 status1");
        let status1 = self.rword()?;
        trace!("[WD] status1: 0x{:04X}", status1);
        if status1 > 0xFF {
            return Err(format!("关闭看门狗失败: status1=0x{:04X}", status1));
        }
        trace!("[WD] 步骤5: echo_4byte(value=0x{:08X})", wdt_value);
        if !self.echo_4byte(wdt_value)? {
            return Err("关闭看门狗: echo value 不匹配".into());
        }
        trace!("[WD] 步骤6: rword() 读 status2");
        let status2 = self.rword()?;
        trace!("[WD] status2: 0x{:04X}", status2);
        if status2 > 0xFF {
            return Err(format!("关闭看门狗失败: status2=0x{:04X}", status2));
        }

        trace!("{}", "看门狗已关闭".green().bold());
        Ok(())
    }

    /// Preloader 模式初始化：握手 + 获取 HW code + 关看门狗
    pub fn init_preloader(&mut self) -> Result<bool, String> {
        // 1. 握手
        if !self.device.do_handshake()? {
            return Ok(false);
        }

        // 2. 获取 HW code 来匹配芯片配置
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知芯片: HW code 0x{:04X}", hw))?;
        self.chip = Some(*chip);

        // 3. 关闭看门狗（Preloader 模式也需要，否则 3 秒后设备自动重启）
        self.disable_watchdog()?;

        trace!("Preloader 模式初始化成功, chip={}", chip.name);
        self.brom_initialized = true;
        self.is_preloader_mode = true;
        Ok(true)
    }

    /// 触发看门狗重启（用于 Preloader Pattern 协议前）
    ///
    /// 串口模式下 brom_register_access(0xDA) 不被设备支持（echo 正常但无 status 响应），
    /// 直接使用 BROM WRITE32 写看门狗 WDT_RESTART 寄存器触发重启。
    ///
    /// 设备重启后 Preloader 发送 READY 信号，Pattern 协议收到后立即发送 FASTBOOT。
    pub fn trigger_meta_reboot(&mut self) -> Result<(), String> {
        let chip = self
            .chip
            .as_ref()
            .ok_or("trigger_meta_reboot: chip 未初始化")?;

        let reboot_addr = chip.watchdog + 0x14;
        let reboot_value: u32 = 0x00001209;

        // 直接 write32 触发看门狗重启（跳过 brom_register_access，串口不支持）
        trace!(
            "[META] write32(0x{:08X}, 0x{:08X}) 触发看门狗重启",
            reboot_addr, reboot_value
        );

        // BROM WRITE32 协议: echo_1byte(0xD4) → echo_4byte(addr) → echo_4byte(count) → echo_4byte(value) → rword()
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

    /// BROM echo 协议：完全对齐 Python Port.echo() (Port.py:210-229)
    /// 设备在上传 DA 后可能输出调试信息，echo 读取时会遇到残留数据。
    /// 这里容忍最多 32 字节的残留数据，继续读取直到找到真正的 echo。
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
            // PIPE 错误（端点 STALL）：clear_halt 后重试一次
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


    /// 发送 4 字节大端参数并校验回显（对齐 Python pack(">I", val)）
    /// 支持残余数据容错：逐字节读取并维护 4 字节滑动窗口环缓冲区，
    /// 跳过残余字节后匹配期望回显（与 echo_1byte 跳过机制一致）。
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

        // 逐字节读取，维护 4 字节滑动窗口环缓冲区
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
                        // 按写入顺序重组环缓冲区为连续 4 字节
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
                            // 还在填充环缓冲区阶段，记录残余字节
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

    /// 发送 4 字节大端参数，校验回显后再读取 2 字节 status。
    /// 对齐刷机匣 watchdog 关闭流程：write 4B -> read 4B echo -> read 2B status。
    #[allow(dead_code)] // 预留：部分 BROM 命令需要 4 字节参数 + 2 字节 status 响应模式
    pub fn echo_4byte_then_status(&mut self, val: u32) -> Result<u16, String> {
        if !self.echo_4byte(val)? {
            return Err(format!("4-byte echo mismatch: 0x{:08X}", val));
        }
        let status = self.rword()?;
        trace!("4-byte status for {:08X}: {:04X}", val, status);
        Ok(status)
    }

    /// 发送 1 字节命令（对应 Python echo(Cmd.XXX.value)）
    pub fn sendcmd(&mut self, cmd: u8) -> Result<bool, String> {
        self.echo_1byte(cmd)
    }

    /// 获取硬件代码
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

    /// 获取目标设备安全配置
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

        // chip 已在 init() step 2 设置，不再重复调用
        Ok(TargetConfig::from_raw(target_config))
    }

    /// 读取 HW Subcode
    /// Python: echo(0xDB) → rbyte(2)
    #[allow(dead_code)] // 预留：部分芯片需要通过 hw_subcode 区分变体
    pub fn get_hw_subcode(&mut self) -> Result<u16, String> {
        if !self.sendcmd(0xDB)? {
            return Err("获取 HW subcode 失败: echo 0xDB 不匹配".into());
        }
        let mut buf = [0u8; 2];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read hw subcode: {}", e))?;
        Ok(u16::from_be_bytes(buf))
    }

    /// 清空输入缓冲（串口模式下丢弃所有待读数据，防止 echo mismatch 后读取错位）
    pub fn flush_input(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH] total discarded {} bytes", total);
        }
    }

    /// 快速 flush：短超时（50ms），用于 read32_brom 前置清理
    /// 比常规 flush 快 4 倍，适用于已确认设备状态正常的场景
    pub(crate) fn flush_input_quick(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_QUICK_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_QUICK_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_QUICK_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH_QUICK] discarded {} bytes", total);
        }
    }

    /// 轮询式 flush：用短超时反复读取，直到 USB 缓冲区为空（设备 ready）
    /// 比固定 sleep 更快 — 设备准备好了就立刻返回
    pub fn flush_input_poll(&mut self, interval: Duration, max_iters: u32) -> Result<(), String> {
        self.device.set_timeout(interval);
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for i in 0..max_iters {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => {
                    // 缓冲区空了，设备 ready
                    break;
                }
            }
            if i == max_iters - 1 {
                trace!("[FLUSH_POLL] 轮询 {} 次, 丢弃 {} bytes", max_iters, total);
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        Ok(())
    }


    /// 读 16 位字（2 字节，big-endian，对齐 Python DeviceHandler.rword(little=False)）
    pub fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

}
