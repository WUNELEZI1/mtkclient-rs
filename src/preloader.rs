use crate::config::{CHIP_CONFIGS, ChipConfig, TargetConfig};
use log::{debug, info};
use std::time::Duration;

/// BROM 传输抽象层 — 统一 USB 和串口的读写接口
pub trait BromTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn set_timeout(&mut self, duration: Duration);
    fn get_timeout(&self) -> Duration;
    fn do_handshake(&mut self) -> Result<bool, String>;

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

    /// 枚举所有 COM 口，找到 MediaTek BROM 设备 (VID=0E8D PID=0003)
    pub fn find_brom_port() -> Option<String> {
        let ports = serialport::available_ports().ok()?;
        for p in &ports {
            if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                && info.vid == 0x0E8D && info.pid == 0x0003 {
                    return Some(p.port_name.clone());
                }
        }
        None
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
        let startcmd = [0xA0u8, 0x0A, 0x50, 0x05];
        for (i, cmd_byte) in startcmd.iter().enumerate() {
            self.write(&[*cmd_byte])?;
            let mut response = [0u8; 1];
            self.read_exact(&mut response)?;
            let expected = !*cmd_byte;
            if response[0] != expected {
                return Err(format!(
                    "串口握手失败 字节 {}: 期望 0x{:02X}, 收到 0x{:02X}",
                    i, expected, response[0]
                ));
            }
        }
        info!("SerialPort BROM 握手成功");
        Ok(true)
    }
}

/// Preloader / BROM protocol handler
pub struct Preloader {
    pub device: Box<dyn BromTransport>,
    pub is_preloader_mode: bool,
    pub chip: Option<ChipConfig>,
}

impl Preloader {
    pub fn new(device: Box<dyn BromTransport>) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            chip: None,
        }
    }

    /// 基础初始化（BROM 握手确认设备可用）
    pub fn init(&mut self) -> Result<bool, String> {
        self.device.do_handshake()
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
                    Ok(false)
                }
            }
            Err(e) => {
                debug!("[ECHO_1] read error for 0x{:02X}: {}", cmd, e);
                Ok(false)
            }
        }
    }

    /// 发送 4 字节大端参数并校验回显，对齐 Python echo(pack(">I", val))
    pub fn echo_4byte(&mut self, val: u32) -> Result<bool, String> {
        let be = val.to_be_bytes();
        self.device
            .write(&be)
            .map_err(|e| format!("echo_4byte write: {}", e))?;
        let mut echo = [0u8; 4];
        self.device
            .read_exact(&mut echo)
            .map_err(|e| format!("echo_4byte read: {}", e))?;
        if echo == be {
            Ok(true)
        } else {
            debug!("[ECHO_4] mismatch: expected {:02X?}, got {:02X?}", be, echo);
            Ok(false)
        }
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
        if status == 0x1D0D {
            return Err("SLA required".into());
        }
        if status > 0xFF {
            return Err(format!("SEND_DA status error: {:04X}", status));
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
        self.device
            .write(&addr.to_be_bytes())
            .map_err(|e| format!("jump_da write addr: {}", e))?;
        let resaddr = self.rdword()?;
        if resaddr != addr {
            return Err(format!(
                "jump_da addr mismatch: expected {:08X}, got {:08X}",
                addr, resaddr
            ));
        }
        let status = self.rword()?;
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
    /// 对齐 Python mtk_preloader.py:721-753
    /// Python:
    ///   echo(b"\xDA") → echo(pack(">I", mode)) → echo(pack(">I", address)) → echo(pack(">I", length))
    ///   status = usbread(2)
    ///   if write: write(data) → if check_status: status2 = usbread(2)
    ///   if read: rbyte(length) → status2 = usbread(2)
    pub fn brom_register_access(
        &mut self,
        address: u32,
        length: u32,
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        let mode: u32 = if data.is_some() { 1 } else { 0 };

        // echo 0xDA 命令（1 字节）
        if !self.echo_1byte(0xDA)? {
            return Err("brom_reg: echo 0xDA 不匹配".into());
        }

        // 发送参数（echo，对齐 Python — 全部使用 echo）
        if !self.echo_4byte(mode)? {
            return Err("brom_reg: echo mode 不匹配".into());
        }
        if !self.echo_4byte(address)? {
            return Err("brom_reg: echo addr 不匹配".into());
        }
        if !self.echo_4byte(length)? {
            return Err("brom_reg: echo len 不匹配".into());
        }

        // 读状态 2 字节
        let mut st = [0u8; 2];
        self.device.read_exact(&mut st).ok();
        debug!("brom_reg status1: {:02X?}", st);
        if st != [0, 0] {
            return Err("brom_reg status err".into());
        }

        if let Some(wdata) = data {
            self.device
                .write(wdata)
                .map_err(|e| format!("brom_reg write data: {}", e))?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device.read_exact(&mut st2).ok();
                debug!("brom_reg status3: {:02X?}", st2);
                if st2 != [0, 0] {
                    return Err("brom_reg status2 err".into());
                }
            }
            Ok(None)
        } else {
            let rdata = self.rbyte(length as usize)?;
            debug!("brom_reg read data: {} bytes", rdata.len());
            let mut st2 = [0u8; 2];
            self.device.read_exact(&mut st2).ok();
            debug!("brom_reg status2: {:02X?}", st2);
            Ok(Some(rdata))
        }
    }

    /// 读 32 位值（BROM 模式）
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        self.brom_register_access(addr, (dwords * 4) as u32, None, true)
            .map(|r| r.unwrap_or_default())
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
