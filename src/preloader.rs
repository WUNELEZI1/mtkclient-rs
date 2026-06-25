use crate::config::{CHIP_CONFIGS, ChipConfig, TargetConfig};
use crate::usb::UsbDevice;
use colored::Colorize;
use log::debug;
use std::time::Duration;

/// BROM 传输抽象层 — 统一 USB 和串口的读写接口
pub trait BromTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn set_timeout(&mut self, duration: Duration);
    fn get_timeout(&self) -> Duration;
    fn do_handshake(&mut self) -> Result<bool, String>;
    fn is_libusb(&self) -> bool;

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
}

/// serialport 实现 BROM 传输
pub struct SerialPortTransport {
    port: Box<dyn serialport::SerialPort>,
    timeout: Duration,
}

impl SerialPortTransport {
    pub fn new(port_name: &str, baud_rate: u32) -> Result<Self, String> {
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
    pub fn find_brom_port() -> Option<String> {
        const INTERVAL_MS: u64 = 200;
        let mut retry = 0;

        loop {
            retry += 1;
            let ports = match serialport::available_ports() {
                Ok(p) => p,
                Err(e) => {
                    debug!("available_ports 失败 (retry {}): {}", retry, e);
                    std::thread::sleep(std::time::Duration::from_millis(INTERVAL_MS));
                    continue;
                }
            };

            if retry == 1 || retry % 25 == 1 {
                debug!("available_ports 返回 {} 个端口 (retry {})", ports.len(), retry);
                for p in &ports {
                    debug!("  {} - {:?}", p.port_name, p.port_type);
                }
            }

            for p in &ports {
                if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
                    // BROM 模式: VID=0E8D PID=0003
                    // Preloader 模式: VID=0E8D PID=2000
                    if info.vid == 0x0E8D && (info.pid == 0x0003 || info.pid == 0x2000) {
                        debug!("找到 MTK COM 口: {} (PID=0x{:04X}, retry {})",
                              p.port_name, info.pid, retry);
                        return Some(p.port_name.clone());
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
        for i in 0..4 {
            self.write(&[startcmd[i]])?;
            let mut buf = [0u8; 1];
            self.read_exact(&mut buf)?;
            let expected = !startcmd[i] & 0xFF;
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
        let wdt_value: u32 = 0x22000064;  // 对齐 Python: wdt==0x10007000 时用 0x22000064

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
        if !self.echo_1byte(0xD8)? {
            return Err("BROM init: echo 0xD8 不匹配".into());
        }
        let mut tc_buf = [0u8; 6];
        self.device
            .read_exact(&mut tc_buf)
            .map_err(|e| format!("BROM init: read target_config: {}", e))?;
        let target_cfg = u32::from_be_bytes([tc_buf[0], tc_buf[1], tc_buf[2], tc_buf[3]]);
        let tc_status = u16::from_be_bytes([tc_buf[4], tc_buf[5]]);
        debug!("BROM target_config: 0x{:08X}, status: 0x{:04X}", target_cfg, tc_status);

        // 5. BROM sync: echo(0xFE) → 读 FE — 对齐刷机匣第 141-145 行
        if !self.echo_1byte(0xFE)? {
            return Err("BROM sync FE 失败".into());
        }
        debug!("BROM 模式同步成功");

        // 6. 进入 ID 读取阶段: echo(0xFF) → 读响应 — 对齐刷机匣第 149-153 行
        self.device
            .write(&[0xFF])
            .map_err(|e| format!("BROM FF write: {}", e))?;
        let mut ff_resp = [0u8; 1];
        self.device
            .read_exact(&mut ff_resp)
            .map_err(|e| format!("BROM FF read: {}", e))?;
        debug!("BROM FF 响应: 0x{:02X}", ff_resp[0]);

        // 7. 读 HW SubCode / HW Ver / SW Ver — 对齐刷机匣第 155-162 行
        if !self.echo_1byte(0xFC)? {
            return Err("BROM FC echo 不匹配".into());
        }
        let mut hw_info = [0u8; 8];
        self.device
            .read_exact(&mut hw_info)
            .map_err(|e| format!("BROM read hw_info: {}", e))?;
        let hw_subcode = u16::from_be_bytes([hw_info[0], hw_info[1]]);
        let hw_ver = u16::from_be_bytes([hw_info[2], hw_info[3]]);
        let sw_ver = u16::from_be_bytes([hw_info[4], hw_info[5]]);
        debug!(
            "BROM HW info: subcode=0x{:04X}, hw_ver=0x{:04X}, sw_ver=0x{:04X}",
            hw_subcode, hw_ver, sw_ver
        );

        debug!("{}", "BROM 初始化完成".green().bold());
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
                    self.flush_input();
                    Ok(false)
                }
            }
            Err(e) => {
                debug!("[ECHO_1] read error for 0x{:02X}: {}", cmd, e);
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

        // echo(0xD7) 命令
        if !self.echo_1byte(0xD7)? {
            return Err("SEND_DA: echo 0xD7 不匹配".into());
        }

        // 发送参数（echo，对齐 Python — 全部使用 echo）
        if !self.echo_4byte(address)? {
            return Err("SEND_DA: echo addr 不匹配".into());
        }
        if !self.echo_4byte(size)? {
            return Err("SEND_DA: echo size 不匹配".into());
        }
        if !self.echo_4byte(sig_len)? {
            return Err("SEND_DA: echo sig_len 不匹配".into());
        }

        // rword() 读状态
        let status = self.rword()?;
        debug!("SEND_DA status: {:04X}", status);
        if status > 0xFF {
            return Err(format!("SEND_DA status error: {:04X}", status));
        }
        if status == 0x1D0D {
            return Err("SLA required".into());
        }

        // upload_data: 发送数据 + ZLP + 读校验和
        let data = dadata;
        let chunk_size = 64;
        let mut pos = 0;
        while pos < data.len() {
            let end = (pos + chunk_size).min(data.len());
            self.device
                .write(&data[pos..end])
                .map_err(|e| format!("upload_data write: {}", e))?;
            pos = end;
        }

        // ZLP (Zero Length Packet) — 对应 Python usbwrite(b"")
        self.device
            .write(&[])
            .map_err(|e| format!("upload_data ZLP: {}", e))?;

        // 等待设备处理
        std::thread::sleep(Duration::from_millis(35));

        // 读校验和 + 状态（Python: rword(2) → 2 个 16-bit big-endian）
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
        if !self.echo_1byte(0xD5)? {
            return Err("jump_da: echo 0xD5 不匹配".into());
        }
        // Python: usbwrite(pack(">I", addr)) — 大端
        self.device
            .write(&addr.to_be_bytes())
            .map_err(|e| format!("jump_da write addr: {}", e))?;
        // Python: rdword() — 大端回读
        let mut echo = [0u8; 4];
        self.device
            .read_exact(&mut echo)
            .map_err(|e| format!("jump_da echo: {}", e))?;
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
        Ok(status == 0)
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

    /// BROM 寄存器访问（DA 注入核心操作）
    /// 对齐刷机匣串口协议：
    ///   cmd(D1) → mode(4B) → address(4B) → length_dwords(4B) → status(2B) → data → status(2B)
    /// mode: 0=read, 1=write
    /// length_dwords: DWORD 数（设备期望的单位）
    pub fn brom_register_access(
        &mut self,
        mode: u32,           // 0=读，1=写
        address: u32,
        length_bytes: u32,   // 字节数，对齐 Python brom_register_access
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        // 命令字节 0xDA：使用 echo 协议（对齐 Python Cmd.brom_register_access.value）
        if !self.echo_1byte(0xDA)? {
            return Err("brom_reg: echo 0xDA 不匹配".into());
        }
        // mode: 0=read, 1=write（4 字节大端，对齐 Python echo(pack(">I", mode))）
        if !self.echo_4byte(mode)? {
            return Err("brom_reg: echo mode 不匹配".into());
        }
        // 地址（大端）
        if !self.echo_4byte(address)? {
            return Err("brom_reg: echo addr 不匹配".into());
        }
        // 长度（大端，字节数，对齐 Python）
        if !self.echo_4byte(length_bytes)? {
            return Err("brom_reg: echo len 不匹配".into());
        }

        // 读状态 2 字节
        let mut st = [0u8; 2];
        self.device
            .read_exact(&mut st)
            .map_err(|e| format!("brom_reg status1: {}", e))?;
        debug!("brom_reg status1: {:02X?}", st);

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
            self.device
                .read_exact(&mut buf)
                .map_err(|e| format!("brom_reg read data: {}", e))?;
            debug!("brom_reg read data: {} bytes", buf.len());
            if check_status {
                let mut st2 = [0u8; 2];
                self.device
                    .read_exact(&mut st2)
                    .map_err(|e| format!("brom_reg status2: {}", e))?;
                debug!("brom_reg status2: {:02X?}", st2);
            }
            Ok(Some(buf))
        }
    }

    /// 读 32 位值（BROM 模式）
    /// 使用 0xD1 协议（无 mode 参数）：cmd → addr → len(dwords) → status1 → data → status2
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        // echo 0xD1 命令（1 字节，对齐 Python echo(Cmd.READ32.value)）
        if !self.echo_1byte(0xD1)? {
            return Err("read32_brom: echo 0xD1 不匹配".into());
        }
        // address (4 字节小端)
        if !self.echo_4byte(addr)? {
            return Err("read32_brom: echo addr 不匹配".into());
        }
        // length in dwords (4 字节小端)
        if !self.echo_4byte(dwords as u32)? {
            return Err("read32_brom: echo len 不匹配".into());
        }
        // 读状态 2 字节（小端）
        let mut st = [0u8; 2];
        self.device
            .read_exact(&mut st)
            .map_err(|e| format!("read32_brom status1: {}", e))?;
        debug!("read32_brom status1: {:02X?}", st);
        // 读取数据
        let bytes = dwords * 4;
        let rdata = self.rbyte(bytes)?;
        debug!("read32_brom read data: {} bytes", rdata.len());
        // 读状态 2 字节（小端）
        let mut st2 = [0u8; 2];
        self.device
            .read_exact(&mut st2)
            .map_err(|e| format!("read32_brom status2: {}", e))?;
        debug!("read32_brom status2: {:02X?}", st2);
        Ok(rdata)
    }

    /// 清空输入缓冲（串口模式下丢弃所有待读数据，防止 echo mismatch 后读取错位）
    pub fn flush_input(&mut self) {
        let mut buf = [0u8; 256];
        loop {
            match self.device.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {} // 继续读取直到为空
            }
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
