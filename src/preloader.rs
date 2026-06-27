use crate::config::{CHIP_CONFIGS, ChipConfig, TargetConfig};
use crate::usb::UsbDevice;
use colored::Colorize;
use log::debug;
use std::fs::OpenOptions;
use std::time::Duration;

/// 验证 COM 口是否真实存在
///
/// serialport 的 available_ports() 可能返回注册表残留的无效端口，
/// 通过 CreateFile 打开 \\.\COMx 来验证端口是否真的可用。
fn verify_port_exists(port_name: &str) -> bool {
    let path = format!("\\\\.\\{}", port_name);
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .is_ok()
}

/// BROM 传输抽象层 — 统一 USB 和串口的读写接口
pub trait BromTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn set_timeout(&mut self, duration: Duration);
    fn get_timeout(&self) -> Duration;
    fn do_handshake(&mut self) -> Result<bool, String>;
    fn is_libusb(&self) -> bool;

    /// 获取 USB VID（仅 UsbDevice 有效，串口返回 None）
    fn get_vid(&self) -> Option<u16> {
        None
    }
    /// 获取 USB PID（仅 UsbDevice 有效，串口返回 None）
    fn get_pid(&self) -> Option<u16> {
        None
    }

    // USB 专属方法 — 默认返回错误，仅 UsbDevice 实现
    fn ctrl_transfer_out(
        &mut self,
        _req_type: u8,
        _req: u8,
        _value: u16,
        _index: u16,
        _data: &[u8],
    ) -> Result<usize, String> {
        Err("ctrl_transfer_out not supported on this transport".to_string())
    }
    fn ctrl_transfer_in(
        &mut self,
        _req_type: u8,
        _req: u8,
        _value: u16,
        _index: u16,
        _length: u16,
    ) -> Result<Vec<u8>, String> {
        Err("ctrl_transfer_in not supported on this transport".to_string())
    }
    fn clear_halt_in(&mut self) -> Result<(), String> {
        Err("clear_halt_in not supported on this transport".to_string())
    }
    fn clear_halt_out(&mut self) -> Result<(), String> {
        Err("clear_halt_out not supported on this transport".to_string())
    }
    fn clear_halt_ep(&mut self, _ep: u8) -> Result<(), String> {
        Err("clear_halt_ep not supported on this transport".to_string())
    }
}

/// BROM 端口检测结果
#[derive(Debug)]
pub enum BromPortResult {
    /// 找到串口驱动的设备，返回 COM 口名称
    SerialPort(String),
    /// 找到 WinUSB 驱动的设备（已安装 libwdi 驱动）
    WinUsbDevice,
}

/// serialport 实现 BROM 传输
pub struct SerialPortTransport {
    port: Box<dyn serialport::SerialPort>,
    timeout: Duration,
}

impl SerialPortTransport {
    pub fn new(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        if !verify_port_exists(port_name) {
            return Err(format!("端口 {} 不存在（注册表残留）", port_name));
        }
        let port = serialport::new(port_name, baud_rate)
            .timeout(Duration::from_millis(1000))
            .open()
            .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;
        Ok(SerialPortTransport {
            port,
            timeout: Duration::from_millis(1000),
        })
    }

    /// 枚举所有 COM 口，找到 MediaTek BROM/Preloader 设备
    /// 无限轮询，每 200ms 扫描一次，直到找到为止
    pub fn find_brom_port() -> Option<BromPortResult> {
        Self::find_brom_port_with_timeout(u64::MAX)
    }

    /// 枚举所有 COM 口，找到 MediaTek BROM/Preloader 设备
    /// 带超时的版本，超时返回 None
    ///
    /// 检测逻辑：
    /// 1. 通过 VID/PID 过滤 MTK 设备 (0x0E8D:0x0003/0x2000)
    /// 2. 通过 SetupAPI 查询设备描述和驱动制造商
    /// 3. 设备描述包含 "MediaTek USB Port" 且驱动制造商包含 "libwdi" → WinUSB 驱动，返回 WinUsbDevice
    /// 4. 设备描述包含 "MediaTek USB Port" 且驱动制造商包含 "MediaTek" → 串口驱动，返回 SerialPort
    pub fn find_brom_port_with_timeout(timeout_ms: u64) -> Option<BromPortResult> {
        const INTERVAL_MS: u64 = 200;
        let max_retries = if timeout_ms == u64::MAX || timeout_ms == 0 {
            usize::MAX
        } else {
            timeout_ms.div_ceil(INTERVAL_MS) as usize
        };
        let mut retry = 0usize;

        loop {
            retry += 1;
            if retry > max_retries {
                debug!("find_brom_port 超时 ({}ms)", timeout_ms);
                return None;
            }

            let ports = match serialport::available_ports() {
                Ok(p) => p,
                Err(e) => {
                    debug!("available_ports 失败 (retry {}): {}", retry, e);
                    std::thread::sleep(std::time::Duration::from_millis(INTERVAL_MS));
                    continue;
                }
            };

            if retry == 1 || retry % 25 == 1 {
                debug!(
                    "available_ports 返回 {} 个端口 (retry {})",
                    ports.len(),
                    retry
                );
                for p in &ports {
                    debug!("  {} - {:?}", p.port_name, p.port_type);
                }
            }

            for p in &ports {
                if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
                    // BROM 模式: VID=0E8D PID=0003
                    // Preloader 模式: VID=0E8D PID=2000
                    if info.vid == 0x0E8D && (info.pid == 0x0003 || info.pid == 0x2000) {
                        if !verify_port_exists(&p.port_name) {
                            debug!("端口 {} 注册表残留但设备已拔出，跳过", p.port_name);
                            continue;
                        }

                        // 通过 SetupAPI 查询设备描述和驱动制造商
                        if let Some(usb_info) = crate::driver::query_com_port_usb_info(&p.port_name) {
                            debug!(
                                "COM 口 {} 设备信息: desc='{}', mfg='{}'",
                                p.port_name, usb_info.device_desc, usb_info.driver_mfg
                            );

                            let desc_lower = usb_info.device_desc.to_lowercase();
                            let mfg_lower = usb_info.driver_mfg.to_lowercase();

                            // 检查是否是 MediaTek USB Port 设备
                            if desc_lower.contains("mediatek usb port") {
                                // 检查驱动制造商
                                if mfg_lower.contains("libwdi") {
                                    // WinUSB 驱动，直接返回 WinUsbDevice（应该用 libusb 直接访问）
                                    debug!(
                                        "端口 {} 使用 WinUSB 驱动 (libwdi)，返回 WinUsbDevice",
                                        p.port_name
                                    );
                                    return Some(BromPortResult::WinUsbDevice);
                                } else if mfg_lower.contains("mediatek") {
                                    // 原始串口驱动，使用
                                    debug!(
                                        "找到 MTK COM 口: {} (PID=0x{:04X}, 串口驱动, retry {})",
                                        p.port_name, info.pid, retry
                                    );
                                    return Some(BromPortResult::SerialPort(p.port_name.clone()));
                                } else {
                                    // 未知驱动，记录但继续使用
                                    debug!(
                                        "端口 {} 使用未知驱动: {}，继续使用",
                                        p.port_name, usb_info.driver_mfg
                                    );
                                    return Some(BromPortResult::SerialPort(p.port_name.clone()));
                                }
                            } else {
                                // 不是 MediaTek USB Port，可能是其他设备，继续使用
                                debug!(
                                    "找到 MTK COM 口: {} (PID=0x{:04X}, retry {})",
                                    p.port_name, info.pid, retry
                                );
                                return Some(BromPortResult::SerialPort(p.port_name.clone()));
                            }
                        } else {
                            // 无法获取设备信息，回退到 VID/PID 匹配
                            debug!(
                                "找到 MTK COM 口: {} (PID=0x{:04X}, 无法获取设备信息, retry {})",
                                p.port_name, info.pid, retry
                            );
                            return Some(BromPortResult::SerialPort(p.port_name.clone()));
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(INTERVAL_MS));
        }
    }
}

impl BromTransport for SerialPortTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        self.port
            .write_all(data)
            .map_err(|e| format!("serial write: {}", e))?;
        Ok(data.len())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.port
            .read_exact(buf)
            .map_err(|e| format!("serial read_exact: {}", e))?;
        Ok(buf.len())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.port
            .read(buf)
            .map_err(|e| format!("serial read: {}", e))
    }

    fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
        let _ = self.port.set_timeout(duration);
    }

    fn get_timeout(&self) -> Duration {
        self.timeout
    }

    fn do_handshake(&mut self) -> Result<bool, String> {
        // BROM 握手协议: 逐字节发送 [A0, 0A, 50, 05]，每字节期望取反回复
        // 参考 mtkclient Port.py 实现
        let startcmd = [0xA0u8, 0x0A, 0x50, 0x05];
        for (i, &cmd) in startcmd.iter().enumerate() {
            self.write(&[cmd])?;
            let mut buf = [0u8; 1];
            self.read_exact(&mut buf)?;
            let expected = !cmd;
            if buf[0] != expected {
                return Err(format!(
                    "握手失败: 字节 {}: 期望 0x{:02X}, 收到 0x{:02X}",
                    i, expected, buf[0]
                ));
            }
        }
        debug!("SerialPort BROM 握手成功");
        Ok(true)
    }

    fn is_libusb(&self) -> bool {
        false
    }
}

/// UsbDevice 实现 BROM 传输
impl BromTransport for UsbDevice {
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        UsbDevice::write(self, data)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        UsbDevice::read_exact(self, buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        UsbDevice::read(self, buf)
    }

    fn set_timeout(&mut self, duration: Duration) {
        UsbDevice::set_timeout(self, duration);
    }

    fn get_timeout(&self) -> Duration {
        UsbDevice::get_timeout(self)
    }

    fn do_handshake(&mut self) -> Result<bool, String> {
        UsbDevice::do_handshake(self)
    }

    fn is_libusb(&self) -> bool {
        true
    }

    fn get_vid(&self) -> Option<u16> {
        Some(self.vid)
    }

    fn get_pid(&self) -> Option<u16> {
        Some(self.pid)
    }

    fn ctrl_transfer_out(
        &mut self,
        req_type: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<usize, String> {
        UsbDevice::ctrl_transfer_out(self, req_type, req, value, index, data)?;
        Ok(data.len())
    }

    fn ctrl_transfer_in(
        &mut self,
        req_type: u8,
        req: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Result<Vec<u8>, String> {
        UsbDevice::ctrl_transfer_in(self, req_type, req, value, index, length)
    }

    fn clear_halt_in(&mut self) -> Result<(), String> {
        UsbDevice::clear_halt_in(self)
    }

    fn clear_halt_out(&mut self) -> Result<(), String> {
        UsbDevice::clear_halt_out(self)
    }

    fn clear_halt_ep(&mut self, ep: u8) -> Result<(), String> {
        UsbDevice::clear_halt_ep(self, ep)
    }
}

/// Preloader / BROM protocol handler
pub struct Preloader {
    pub device: Box<dyn BromTransport>,
    pub is_preloader_mode: bool,
    pub chip: Option<ChipConfig>,
    /// BROM 初始化是否完成（init 成功后为 true）
    pub brom_initialized: bool,
}

impl Preloader {
    pub fn new(device: Box<dyn BromTransport>) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            chip: None,
            brom_initialized: false,
        }
    }

    /// 判断是否已经进入 BROM 模式
    ///
    /// 条件：init() 成功完成（握手 + 看门狗 + 同步 + HW info）
    pub fn is_brom_ready(&self) -> bool {
        self.brom_initialized
    }

    /// BROM 同步序列 (FE, FF, FC)
    /// 用于在握手或漏洞利用后让设备进入就绪状态
    pub fn sync_brom(&mut self) -> Result<(), String> {
        debug!("开始 BROM 同步序列...");

        // 1. BROM sync: echo(0xFE) -> 读 FE
        if !self.echo_1byte(0xFE)? {
            return Err("BROM sync FE 失败".into());
        }
        debug!("BROM sync (0xFE) OK");

        // 2. BROM FF: echo(0xFF) -> 读响应
        self.device
            .write(&[0xFF])
            .map_err(|e| format!("BROM FF write: {}", e))?;
        let mut ff_resp = [0u8; 1];
        self.device
            .read_exact(&mut ff_resp)
            .map_err(|e| format!("BROM FF read: {}", e))?;
        debug!("BROM FF 响应: 0x{:02X}", ff_resp[0]);

        // 3. BROM FC: echo(0xFC) -> 读 8 字节 HW info
        if !self.echo_1byte(0xFC)? {
            return Err("BROM FC echo 不匹配".into());
        }
        let mut hw_info = [0u8; 8];
        self.device
            .read_exact(&mut hw_info)
            .map_err(|e| format!("BROM read hw_info: {}", e))?;
        debug!("BROM HW info (FC) OK: {:02X?}", hw_info);

        Ok(())
    }

    /// 完整初始化：握手 + 关闭看门狗 + 读取设备信息
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

        // 3. 关闭看门狗（对齐 Python write32 协议）
        // Python 调用: echo(Cmd.WRITE32.value) → echo(pack(">I", addr)) → echo(pack(">I", 1)) → rword() → echo(pack(">I", value)) → rword()
        // Cmd.WRITE32.value = b"\xD4" — 作为 bytes 只发送 1 字节
        debug!("[WD] 开始关闭看门狗流程 (write32协议)");
        self.device.set_timeout(Duration::from_secs(3));

        let wdt_addr = chip.watchdog;
        let wdt_value: u32 = 0x22000064; // 对齐 Python: wdt==0x10007000 时用 0x22000064

        debug!("[WD] 地址=0x{:08X}, value=0x{:08X}", wdt_addr, wdt_value);

        // 步骤 1: echo(0xD4) — 发送 1 字节命令
        debug!("[WD] 步骤1: echo_1byte(0xD4)");
        if !self.echo_1byte(0xD4)? {
            return Err("关闭看门狗: echo 0xD4 不匹配".into());
        }
        // 步骤 2: echo(addr) — 4 字节大端地址
        debug!("[WD] 步骤2: echo_4byte(addr=0x{:08X})", wdt_addr);
        if !self.echo_4byte(wdt_addr)? {
            return Err("关闭看门狗: echo addr 不匹配".into());
        }
        // 步骤 3: echo(1) — 写入 1 个值
        debug!("[WD] 步骤3: echo_4byte(count=1)");
        if !self.echo_4byte(1)? {
            return Err("关闭看门狗: echo count 不匹配".into());
        }
        // 步骤 4: 读 status1 (应该 <= 3 表示 OK)
        debug!("[WD] 步骤4: rword() 读 status1");
        let status1 = self.rword()?;
        debug!("[WD] status1: 0x{:04X}", status1);
        if status1 > 0xFF {
            return Err(format!("关闭看门狗失败: status1=0x{:04X}", status1));
        }
        // 步骤 5: echo(wdt_value) — 4 字节大端值
        debug!("[WD] 步骤5: echo_4byte(value=0x{:08X})", wdt_value);
        if !self.echo_4byte(wdt_value)? {
            return Err("关闭看门狗: echo value 不匹配".into());
        }
        // 步骤 6: 读 status2 (应该 <= 0xFF 表示成功)
        debug!("[WD] 步骤6: rword() 读 status2");
        let status2 = self.rword()?;
        debug!("[WD] status2: 0x{:04X}", status2);
        if status2 > 0xFF {
            return Err(format!("关闭看门狗失败: status2=0x{:04X}", status2));
        }

        debug!("{}", "看门狗已关闭".green().bold());

        // 4. get_target_config — 对齐刷机匣第 101-108 行
        let _ = self.get_target_config();

        // 5-7. BROM sync (FE, FF, FC)
        self.sync_brom()?;

        debug!("BROM 模式初始化成功");
        self.brom_initialized = true;
        Ok(true)
    }

    /// BROM echo 协议：完全对齐 Python Port.echo() (Port.py:210-229)
    /// Python 逻辑：
    ///   if isinstance(data, int):
    ///       data = pack(">I", data)    # int → 4 字节大端
    ///   if isinstance(data, bytes):
    ///       data = [data]               # bytes → [bytes] 列表
    ///   for val in data:
    ///       self.usbwrite(val)
    ///       tmp = self.usbread(len(val), maxtimeout=0)
    ///       if val != tmp:
    ///           return False
    ///   return True
    ///
    /// Rust 实现：
    ///   - sendcmd(u8) 调用 echo_1byte 发送 1 字节命令（对应 Python Cmd enum bytes）
    ///   - echo_4byte(u32) 发送 4 字节大端参数（对应 Python pack(">I", val)）
    pub fn echo_1byte(&mut self, cmd: u8) -> Result<bool, String> {
        // 串口协议：发送 1 字节，读回 1 字节（握手阶段）
        self.device.set_timeout(Duration::from_millis(1000));
        self.device
            .write(&[cmd])
            .map_err(|e| format!("echo write: {}", e))?;
        let mut buf = [0u8; 1];
        match self.device.read_exact(&mut buf) {
            Ok(_) => {
                if buf[0] == cmd {
                    Ok(true)
                } else {
                    debug!(
                        "[ECHO_1] mismatch: expected 0x{:02X}, got 0x{:02X}",
                        cmd, buf[0]
                    );
                    // 不匹配时清空输入缓冲，防止后续读取错位
                    // 但不延迟，立即返回让调用者重试
                    self.flush_input();
                    Ok(false)
                }
            }
            Err(e) => {
                debug!("[ECHO_1] read error for 0x{:02X}: {}", cmd, e);
                // 不 flush_input，让调用者控制重试策略
                Ok(false)
            }
        }
    }

    /// 发送 1 字节命令（4 字节小端），读回 4 字节回显（用于 brom_register_access / read32_brom）
    pub fn echo_cmd_4byte(&mut self, cmd: u8) -> Result<bool, String> {
        // 对齐 Python: echo(cmd) 发送单字节命令
        // 但有些设备需要 4 字节格式，这里尝试两种方式
        let le_bytes = [cmd, 0, 0, 0];
        debug!("[ECHO_CMD_4] 发送: {:02X?}", le_bytes);
        self.device
            .write(&le_bytes)
            .map_err(|e| format!("echo_cmd_4byte write: {}", e))?;
        let mut buf = [0u8; 4];
        match self.device.read_exact(&mut buf) {
            Ok(_) => {
                debug!("[ECHO_CMD_4] 接收: {:02X?} (期望 {:02X?})", buf, le_bytes);
                if buf == le_bytes {
                    Ok(true)
                } else {
                    debug!(
                        "[ECHO_CMD_4] mismatch: expected {:02X?}, got {:02X?}",
                        le_bytes, buf
                    );
                    self.flush_input();
                    Ok(false)
                }
            }
            Err(e) => {
                debug!("[ECHO_CMD_4] read error for 0x{:02X}: {}", cmd, e);
                Ok(false)
            }
        }
    }

    /// 发送 4 字节大端参数并校验回显（对齐 Python pack(">I", val)）
    pub fn echo_4byte(&mut self, val: u32) -> Result<bool, String> {
        // Python 使用大端: pack(">I", val)
        let be = val.to_be_bytes();
        debug!("[ECHO_4] 发送: {:02X?} (值=0x{:08X})", be, val);
        self.device
            .write(&be)
            .map_err(|e| format!("echo_4byte write: {}", e))?;
        let mut echo = [0u8; 4];
        self.device
            .read_exact(&mut echo)
            .map_err(|e| format!("echo_4byte read: {}", e))?;
        debug!("[ECHO_4] 接收: {:02X?} (期望 {:02X?})", echo, be);
        if echo == be {
            Ok(true)
        } else {
            debug!("[ECHO_4] mismatch: expected {:02X?}, got {:02X?}", be, echo);
            Ok(false)
        }
    }

    /// 发送 4 字节大端参数，校验回显后再读取 2 字节 status。
    /// 对齐刷机匣 watchdog 关闭流程：write 4B -> read 4B echo -> read 2B status。
    /// 调用方负责检查返回的 status 是否为 0x0001。
    #[allow(dead_code)] // 预留：部分 BROM 命令需要 4 字节参数 + 2 字节 status 响应模式
    pub fn echo_4byte_then_status(&mut self, val: u32) -> Result<u16, String> {
        if !self.echo_4byte(val)? {
            return Err(format!("4-byte echo mismatch: 0x{:08X}", val));
        }
        let status = self.rword()?;
        debug!("4-byte status for {:08X}: {:04X}", val, status);
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
        debug!("HW code: {:04X}", hw);
        Ok(hw)
    }

    /// 获取目标设备安全配置
    /// Python mtk_preloader.py:517-542: echo(0xD8) → rbyte(6) → unpack(">IH")
    pub fn get_target_config(&mut self) -> Result<TargetConfig, String> {
        // 对齐 Python：先发 0xD8 命令，再读 6 字节
        if !self.sendcmd(0xD8)? {
            return Err("获取 target config 失败: echo 0xD8 不匹配".into());
        }

        let mut buf = [0u8; 6];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read target config: {}", e))?;
        let target_config = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let status = u16::from_be_bytes([buf[4], buf[5]]);
        debug!(
            "Target config: {:08X}, status: {:04X}",
            target_config, status
        );
        if status > 0xFF {
            return Err(format!("Get Target Config Error: status=0x{:04X}", status));
        }

        // 读取硬件码并匹配芯片（Python init() 中在 get_target_config 之后调用）
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知 HW code: {:04X}", hw))?;
        self.chip = Some(*chip);

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

    /// SEND_DA: 发送 Download Agent 到设备
    /// 对齐 Python mtk_preloader.py:871-906
    /// Python:
    ///   echo(Cmd.SEND_DA.value)  → echo(addr) → echo(len(data)) → echo(sig_len)
    ///   status = rword()
    ///   if status ok: upload_data(data, gen_chksum)
    ///
    /// 注意：mtkclient 2.0.1 在 upload_data 之前没有 clear_halt 也没有 warm-up ZLP，
    /// 但 pyusb 内部在 write 超时后会自动处理端点停止。libusb 需要显式 clear_halt。
    /// SEND_DA: 发送 Download Agent 到设备
    /// 对齐 Python mtkclient 2.0.1 的实现
    pub fn send_da(
        &mut self,
        address: u32,
        size: u32,
        sig_len: u32,
        dadata: &[u8],
    ) -> Result<bool, String> {
        debug!(
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
        debug!("SEND_DA status: {:04X}", status);
        if status > 0xFF {
            return Err(format!("SEND_DA status error: {:04X}", status));
        }
        if status == 0x1D0D {
            return Err("SLA required".into());
        }

        // 4. 上传数据
        debug!("[UPLOAD] sending {} bytes in chunks of 512", dadata.len());

        // 4a. 清除端点状态（对应 pyUSB 自动处理的 clear_halt）
        if self.device.is_libusb() {
            debug!("[UPLOAD] clear_halt_out before upload");
            let _ = self.device.clear_halt_out(); // 忽略错误，继续
        }

        // 4b. 设置超时
        let orig_timeout = self.device.get_timeout();
        self.device.set_timeout(Duration::from_millis(5000));

        // 4c. 发送数据（64 字节块，对齐 Python upload_data）
        let data = dadata;
        let chunk_size: usize = 64;
        let mut pos = 0;

        while pos < data.len() {
            let end = (pos + chunk_size).min(data.len());
            self.device
                .write(&data[pos..end])
                .map_err(|e| format!("upload_data write (pos={}): {}", pos, e))?;
            pos = end;
        }

        // 4d. 所有数据发完后，发一次 ZLP（pyUSB 自动做，libusb 需要手动）
        debug!("[UPLOAD] sending ZLP");
        self.device
            .write(&[])
            .map_err(|e| format!("upload_data ZLP: {}", e))?;

        // 4e. 等待设备处理（对齐 Python: time.sleep(0.035)）
        std::thread::sleep(Duration::from_millis(35));

        // 4f. 恢复超时
        self.device.set_timeout(orig_timeout);

        // 5. 读校验和 + 状态（Python: rword(2)）
        let checksum = self.rword()?;
        let status2 = self.rword()?;
        debug!(
            "SEND_DA checksum: {:04X}, status2: {:04X}",
            checksum, status2
        );

        Ok(true)
    }

    /// JUMP_DA: 跳转到 Download Agent
    /// Python: echo(JUMP_DA) → usbwrite(pack(">I", addr)) → rdword() → rword()
    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        // send_da 后设备需要时间切换到 DA 模式
        let mut last_err = String::new();
        for attempt in 1..=5 {
            // 等待设备从 send_da 状态恢复
            if attempt == 1 {
                std::thread::sleep(Duration::from_millis(100));
            } else {
                std::thread::sleep(Duration::from_millis(200));
            }

            // 每次重试前清理 USB 端点
            if self.device.is_libusb() {
                let _ = self.device.clear_halt_in();
                let _ = self.device.clear_halt_out();
            }
            self.flush_input();

            if !self.echo_1byte(0xD5)? {
                last_err = "jump_da: echo 0xD5 不匹配".to_string();
                debug!("[JUMP_DA] attempt {}: echo 0xD5 不匹配，重试", attempt);
                continue;
            }
            // Python: usbwrite(pack(">I", addr)) — 大端
            if let Err(e) = self.device.write(&addr.to_be_bytes()) {
                last_err = format!("jump_da write addr: {}", e);
                debug!("[JUMP_DA] attempt {}: write addr 失败: {}，重试", attempt, e);
                self.flush_input();
                if self.device.is_libusb() {
                    let _ = self.device.clear_halt_in();
                    let _ = self.device.clear_halt_out();
                }
                std::thread::sleep(Duration::from_millis(50));
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
                    // Python: rword() — 大端状态
                    let mut st = [0u8; 2];
                    self.device
                        .read_exact(&mut st)
                        .map_err(|e| format!("jump_da status: {}", e))?;
                    let status = u16::from_be_bytes(st);
                    // Python v2.1.4.1: time.sleep(0.1) after rword() — fix rare timing issue
                    std::thread::sleep(Duration::from_millis(100));
                    debug!("jump_da status: {:04X}", status);
                    return Ok(status == 0);
                }
                Err(e) => {
                    last_err = format!("jump_da echo: {}", e);
                    debug!("[JUMP_DA] attempt {}: read echo 失败: {}，重试", attempt, e);
                    self.flush_input();
                    if self.device.is_libusb() {
                        let _ = self.device.clear_halt_in();
                        let _ = self.device.clear_halt_out();
                    }
                    std::thread::sleep(Duration::from_millis(50));
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
        debug!("jump_bl status: {:04X}", status);
        if status <= 0xFF {
            let status2 = self.rword()?;
            debug!("jump_bl status2: {:04X}", status2);
            Ok(status2 <= 0xFF)
        } else {
            Ok(false)
        }
    }

    /// 获取 ME_ID (0xE1)
    pub fn get_me_id(&mut self) -> Result<Vec<u8>, String> {
        // 对齐 Python: echo(0xFE) -> echo(0xE1)
        if !self.echo_1byte(0xFE)? {
            return Err("get_me_id: sync FE 失败".into());
        }
        if !self.echo_1byte(0xE1)? {
            return Err("get_me_id: echo 0xE1 失败".into());
        }
        // 读 4 字节长度 (BE)
        let mut len_buf = [0u8; 4];
        self.device.read_exact(&mut len_buf)?;
        let length = u32::from_be_bytes(len_buf) as usize;
        // 读 ME_ID 数据
        let mut data = vec![0u8; length];
        self.device.read_exact(&mut data)?;
        // 读 2 字节状态 (BE)
        let _status = self.rword()?;
        debug!("ME_ID: {:02X?}", data);
        Ok(data)
    }

    /// 获取 SOC_ID (0xE7)
    pub fn get_soc_id(&mut self) -> Result<Vec<u8>, String> {
        // 对齐 Python: echo(0xFE) -> echo(0xE7)
        if !self.echo_1byte(0xFE)? {
            return Err("get_soc_id: sync FE 失败".into());
        }
        if !self.echo_1byte(0xE7)? {
            return Err("get_soc_id: echo 0xE7 失败".into());
        }
        // 读 4 字节长度 (BE)
        let mut len_buf = [0u8; 4];
        self.device.read_exact(&mut len_buf)?;
        let length = u32::from_be_bytes(len_buf) as usize;
        // 读 SOC_ID 数据
        let mut data = vec![0u8; length];
        self.device.read_exact(&mut data)?;
        // 读 2 字节状态 (BE)
        let _status = self.rword()?;
        debug!("SOC_ID: {:02X?}", data);
        Ok(data)
    }

    /// BROM 寄存器访问（DA 注入核心操作）
    /// 对齐刷机匣串口协议：
    ///   cmd(D1) → mode(4B) → address(4B) → length_dwords(4B) → status(2B) → data → status(2B)
    /// mode: 0=read, 1=write
    /// length_dwords: DWORD 数（设备期望的单位）
    pub fn brom_register_access(
        &mut self,
        mode: u32, // 0=读，1=写
        address: u32,
        length_bytes: u32, // 字节数，对齐 Python brom_register_access
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        // 命令字节 0xDA：使用 echo 协议（对齐 Python Cmd.brom_register_access.value）
        // 注意：Python 不检查 echo 返回值，这里也只记录警告不返回错误
        if !self.echo_1byte(0xDA)? {
            debug!("brom_reg: echo 0xDA 不匹配（继续执行）");
        }
        // mode: 0=read, 1=write（4 字节大端，对齐 Python echo(pack(">I", mode))）
        if !self.echo_4byte(mode)? {
            debug!("brom_reg: echo mode 不匹配（继续执行）");
        }
        // 地址（大端）
        if !self.echo_4byte(address)? {
            debug!("brom_reg: echo addr 不匹配（继续执行）");
        }
        // 长度（大端，字节数，对齐 Python）
        if !self.echo_4byte(length_bytes)? {
            debug!("brom_reg: echo len 不匹配（继续执行）");
        }

        // 读状态 2 字节（容错模式，对齐 Python 的宽松风格）
        let mut st = [0u8; 2];
        if let Err(e) = self.device.read_exact(&mut st) {
            debug!("brom_reg status1 read failed: {}, continue", e);
        } else {
            debug!("brom_reg status1: {:02X?}", st);
        }

        if let Some(wdata) = data {
            // Write mode: 发送 length_bytes 字节数据后读 status2
            let byte_count = length_bytes as usize;
            self.device
                .write(&wdata[..byte_count])
                .map_err(|e| format!("brom_reg write data: {}", e))?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device
                    .read_exact(&mut st2)
                    .map_err(|e| format!("brom_reg status2: {}", e))?;
                debug!("brom_reg status2: {:02X?}", st2);
            }
            Ok(None)
        } else {
            // Read mode: 读取 length_bytes 字节
            let byte_count = length_bytes as usize;
            let mut buf = vec![0u8; byte_count];

            debug!("brom_reg 准备读 {} 字节数据", byte_count);

            // 关键修复：增加超时 + 重试 + clear_halt
            self.device.set_timeout(Duration::from_millis(10000));
            let _ = self.device.clear_halt_in();

            // 多次尝试读数据
            let mut success = false;
            for attempt in 0..4 {
                match self.device.read_exact(&mut buf) {
                    Ok(n) if n == byte_count => {
                        debug!("brom_reg read data 成功: {} 字节", n);
                        success = true;
                        break;
                    }
                    _ => {
                        debug!("brom_reg read attempt {} failed, flush...", attempt+1);
                        self.flush_input();
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }

            if !success {
                return Err("brom_reg read data timeout after retries".into());
            }

            // 读 status2
            let mut st2 = [0u8; 2];
            let _ = self.device.read_exact(&mut st2);  // 允许失败
            debug!("brom_reg status2: {:02X?}", st2);

            Ok(Some(buf))
        }
    }

    /// 读 32 位值（BROM 模式）
    /// 使用 0xD1 协议（无 mode 参数）：cmd → addr → len(dwords) → status1 → data → status2
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        // 新增：读数据前强制 flush + clear_halt
        self.flush_input();
        std::thread::sleep(Duration::from_millis(20));

        // 在执行关键 BROM 命令前，给予设备微小的准备时间
        if self.device.is_libusb() {
            // 注意：不要在此处调用 clear_halt，因为在 libusb-win32 下它可能导致 5 秒以上的驱动超时
            // 之前的 bypass_security 已经完成了 drain 和 handshake，此处通信应是同步的
            std::thread::sleep(Duration::from_millis(10));
        }

        // echo 0xD1 命令（1 字节，对齐 Python echo(Cmd.READ32.value)）
        debug!("[read32_brom] 发送命令 0xD1");
        if !self.echo_1byte(0xD1)? {
            debug!("read32_brom: echo 0xD1 不匹配（继续尝试）");
        }
        // address (4 字节大端)
        debug!("[read32_brom] 发送地址 0x{:08X}", addr);
        if !self.echo_4byte(addr)? {
            debug!("read32_brom: echo addr 不匹配（继续尝试）");
        }
        // length in dwords (4 字节大端)
        debug!("[read32_brom] 发送长度 {} dwords", dwords);
        if !self.echo_4byte(dwords as u32)? {
            debug!("read32_brom: echo len 不匹配（继续尝试）");
        }
        // 读状态 2 字节（大端）
        let mut st = [0u8; 2];
        self.device
            .read_exact(&mut st)
            .map_err(|e| format!("read32_brom status1: {}", e))?;
        debug!("read32_brom status1: {:02X?}", st);

        // 读取数据
        let bytes = dwords * 4;
        let mut rdata = vec![0u8; bytes];
        self.device
            .read_exact(&mut rdata)
            .map_err(|e| format!("read32_brom data ({} bytes): {}", bytes, e))?;
        debug!("read32_brom read data: {} bytes", rdata.len());

        // 读状态 2 字节（大端）
        let mut st2 = [0u8; 2];
        self.device
            .read_exact(&mut st2)
            .map_err(|e| format!("read32_brom status2: {}", e))?;
        debug!("read32_brom status2: {:02X?}", st2);
        Ok(rdata)
    }

    /// 清空输入缓冲（串口模式下丢弃所有待读数据，防止 echo mismatch 后读取错位）
    /// 加强版：最多尝试 15 次，每次 20ms 超时，确保彻底清空
    pub fn flush_input(&mut self) {
        self.device.set_timeout(Duration::from_millis(30));
        let mut trash = [0u8; 1024];
        let mut total = 0;
        for _ in 0..20 {   // 增加循环次数
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device.set_timeout(Duration::from_millis(5000));
        if total > 0 {
            debug!("[FLUSH] total discarded {} bytes", total);
        }
    }

    /// 读 n 字节
    pub fn rbyte(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("rbyte({}): {}", n, e))?;
        Ok(buf)
    }

    /// 读 16 位字（2 字节，big-endian，对齐 Python DeviceHandler.rword(little=False)）
    pub fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    /// 读 32 位双字（4 字节，big-endian，对齐 Python DeviceHandler.rdword(little=False)）
    pub fn rdword(&mut self) -> Result<u32, String> {
        let mut buf = [0u8; 4];
        self.device.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }
}
