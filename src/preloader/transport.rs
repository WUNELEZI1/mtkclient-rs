//! BROM 传输抽象层 — 统一 USB 和串口的读写接口
//!
//! - `BromTransport` trait — 所有 BROM 设备共用的协议原语
//! - `SerialPortTransport` — Windows COM 口实现
//! - `UsbDevice` 的 `BromTransport` impl — LibUSB 实现
//!
//! 设计要点：
//! 1. USB 专属方法（ctrl_transfer / clear_halt）在 trait 上有默认错误实现，
//!    避免在串口实现中重复冗余代码。
//! 2. 串口实现负责扫描可用 COM 口并区分 WinUSB / 串口驱动的 BROM 设备。

use crate::usb::USB设备;
use log::trace;
use std::fs::OpenOptions;
use std::time::Duration;

const SERIAL_OPEN_TIMEOUT_MS: u64 = 1000;
const FIND_BROM_INTERVAL_MS: u64 = 200;
const SERIAL_HANDSHAKE_BYTES: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];

/// 验证 COM 口是否真实存在
///
/// serialport 的 available_ports() 可能返回注册表残留的无效端口，
/// 通过 CreateFile 打开 \\.\COMx 来验证端口是否真的可用。
pub(crate) fn verify_port_exists(port_name: &str) -> bool {
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
    fn submit_read_request(&mut self, _len: usize) -> Result<bool, String> {
        Ok(false)
    }
    fn complete_read_request(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
        Err("queued read not supported on this transport".to_string())
    }
    fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn cancel_pending_transfers(&mut self) {}
    /// 清空 USB IN pending data，写入前调用防止读取残留干扰
    fn drain_pending(&mut self) {}
    /// 循环排空 IN 管道残留（不 clear_halt，用于 DA 会话复用）
    fn drain_pipes(&mut self) {}
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

    /// 获取 EP_OUT 最大包大小（对齐 Python usblib.write 的 pktsize）
    fn 获取输出端点最大包大小(&self) -> u16 {
        512 // 默认回退值，USB 实现会覆盖为真实值
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

    /// USB 总线复位（默认不支持，仅 UsbDevice 实现）
    fn reset_device(&mut self) -> Result<(), String> {
        Err("reset_device not supported on this transport".to_string())
    }

    /// 关闭 USB 设备（默认不支持，仅 UsbDevice 实现）
    fn close_device(&mut self) -> Result<(), String> {
        Err("close_device not supported on this transport".to_string())
    }

    /// 重新打开 USB 设备（默认不支持，仅 UsbDevice 实现）
    fn reopen_device(&mut self, _context: &crate::usb::USB上下文) -> Result<(), String> {
        Err("reopen_device not supported on this transport".to_string())
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
    port: Option<Box<dyn serialport::SerialPort>>,
    timeout: Duration,
}

impl SerialPortTransport {
    pub fn new(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        if !verify_port_exists(port_name) {
            return Err(format!("端口 {} 不存在（注册表残留）", port_name));
        }
        Self::open_raw(port_name, baud_rate)
    }

    /// 直接打开串口，不做 verify_port_exists 验证
    ///
    /// Pattern 协议必须使用此方法！verify_port_exists 会打开再关闭 COM 口，
    /// 清空串口接收缓冲区中 Preloader 发送的 READY 信号。
    ///
    /// 对齐 Python SerialClass.connect(): dsrdtr=False, rtscts=False
    pub fn open_raw(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        let mut port = serialport::new(port_name, baud_rate)
            .timeout(Duration::from_millis(SERIAL_OPEN_TIMEOUT_MS))
            .data_bits(serialport::DataBits::Eight)
            .stop_bits(serialport::StopBits::One)
            .parity(serialport::Parity::None)
            .flow_control(serialport::FlowControl::None)
            .open()
            .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;
        // 对齐 Python: dsrdtr=False — 禁用 DTR 信号
        // DTR 信号会触发设备进入 BROM 握手模式
        let _ = port.write_data_terminal_ready(false);
        trace!(
            "[SERIAL] {} 已打开(raw, DTR=off): baud={}, 8N1, no_flow_control",
            port_name, baud_rate
        );
        // 打开后等待设备固件/驱动就绪的短延时，降低弱连接下“端口刚出现即握手”
        // 的竞态（设备枚举后需要数十毫秒稳定）。仅 sleep，不清空接收缓冲区，
        // 故不影响 Pattern 协议的 READY 信号。
        std::thread::sleep(Duration::from_millis(150));
        Ok(SerialPortTransport {
            port: Some(port),
            timeout: Duration::from_millis(SERIAL_OPEN_TIMEOUT_MS),
        })
    }

    /// 枚举所有 COM 口，找到 MediaTek BROM/Preloader 设备
    /// 无限轮询，每 200ms 扫描一次，直到找到为止
    pub fn find_brom_port() -> Option<BromPortResult> {
        Self::find_brom_port_with_timeout(u64::MAX)
    }

    /// 枚举所有 COM 口，找到 MediaTek BROM/Preloader 设备
    /// 带超时的版本，超时返回 None
    pub fn find_brom_port_with_timeout(timeout_ms: u64) -> Option<BromPortResult> {
        let max_retries = if timeout_ms == u64::MAX || timeout_ms == 0 {
            usize::MAX
        } else {
            timeout_ms.div_ceil(FIND_BROM_INTERVAL_MS) as usize
        };
        let mut retry = 0usize;

        loop {
            retry += 1;
            if retry > max_retries {
                trace!("find_brom_port 超时 ({}ms)", timeout_ms);
                return None;
            }

            let ports = match serialport::available_ports() {
                Ok(p) => p,
                Err(e) => {
                    trace!("available_ports 返回 (retry {}): {}", retry, e);
                    std::thread::sleep(std::time::Duration::from_millis(FIND_BROM_INTERVAL_MS));
                    continue;
                }
            };

            if retry == 1 || retry % 25 == 1 {
                trace!(
                    "available_ports 返回 {} 个端口 (retry {})",
                    ports.len(),
                    retry
                );
                for p in &ports {
                    trace!("  {} - {:?}", p.port_name, p.port_type);
                }
            }

            for p in &ports {
                if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
                    // BROM 模式: VID=0E8D PID=0003
                    // Preloader 模式: VID=0E8D PID=2000
                    if info.vid == 0x0E8D && (info.pid == 0x0003 || info.pid == 0x2000) {
                        if !verify_port_exists(&p.port_name) {
                            trace!("端口 {} 注册表残留但设备已拔出，跳过", p.port_name);
                            continue;
                        }

                        if let Some(usb_info) =
                            crate::connection::driver::query_com_port_usb_info(&p.port_name)
                        {
                            trace!(
                                "COM 口 {} 设备信息: desc='{}', mfg='{}'",
                                p.port_name, usb_info.device_desc, usb_info.driver_mfg
                            );

                            let desc_lower = usb_info.device_desc.to_lowercase();
                            let mfg_lower = usb_info.driver_mfg.to_lowercase();

                            if desc_lower.contains("mediatek usb port") {
                                if mfg_lower.contains("libwdi") {
                                    trace!(
                                        "端口 {} 使用 WinUSB 驱动 (libwdi)，返回 WinUsbDevice",
                                        p.port_name
                                    );
                                    return Some(BromPortResult::WinUsbDevice);
                                } else if mfg_lower.contains("mediatek") {
                                    trace!(
                                        "找到 MTK COM 口: {} (PID=0x{:04X}, 串口驱动, retry {})",
                                        p.port_name, info.pid, retry
                                    );
                                    return Some(BromPortResult::SerialPort(p.port_name.clone()));
                                } else {
                                    trace!(
                                        "端口 {} 使用未知驱动: {}，继续使用",
                                        p.port_name, usb_info.driver_mfg
                                    );
                                    return Some(BromPortResult::SerialPort(p.port_name.clone()));
                                }
                            } else {
                                trace!(
                                    "找到 MTK COM 口: {} (PID=0x{:04X}, retry {})",
                                    p.port_name, info.pid, retry
                                );
                                return Some(BromPortResult::SerialPort(p.port_name.clone()));
                            }
                        } else {
                            trace!(
                                "找到 MTK COM 口: {} (PID=0x{:04X}, 无法获取设备信息, retry {})",
                                p.port_name, info.pid, retry
                            );
                            return Some(BromPortResult::SerialPort(p.port_name.clone()));
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(FIND_BROM_INTERVAL_MS));
        }
    }
}

/// 串口握手内核：逐字节发送 [A0,0A,50,05]，期望逐字节取反回复。
///
/// 与 usb/device_handshake.rs 的 USB 握手保持一致的健壮性：
/// 多次重试 + 间隔 + 排空 + 错位计数重置(i=0) + 残留DA流检测。
/// 设备进入握手态前可能持续发出杂散字节/文本（典型：期望 0x5F 却读到 0x52 'R'），
/// 故需容忍错位并在最终失败时回显原始字节，便于定位根因：
///   - 连续 0x52('R') 等文本字节 → 设备未在握手态（可能阶段/驱动/DTR 问题）
///   - 收到 0x5F 0x0F 0xA0 0x0A → 表明是 MTK 标准同步变体（非逐字节取反）
fn serial_do_handshake(
    transport: &mut SerialPortTransport,
    max_attempts: u32,
    retry_delay_ms: u64,
    max_mismatch: u32,
    residual_window: usize,
) -> Result<bool, String> {
    let mut last_received: Vec<u8> = Vec::with_capacity(16);

    for attempt in 0..max_attempts {
        if crate::cancel::force_requested() || crate::cancel::requested() {
            return Err("握手已取消".to_string());
        }
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(retry_delay_ms));
        }

        // 仅在重试时排空残留；首次 attempt 必须保留接收缓冲中的 Preloader "READY" 同步
        // 信号（设备进入握手态时配合主机节奏逐字节输出 READY，刷机匣成功日志
        // 0x52/45/41/44/59 = "READY"）。若首 attempt 即 drain，会清掉这些字节，
        // 导致后续握手"未收到任何响应字节"。
        if attempt > 0 {
            transport.drain_pipes();
        }

        let mut ok = true;
        let mut mismatch: u32 = 0;
        let mut residual: Vec<u8> = Vec::with_capacity(residual_window);
        last_received.clear();
        let mut i = 0usize;
        while i < SERIAL_HANDSHAKE_BYTES.len() {
            if let Err(e) = transport.write(&[SERIAL_HANDSHAKE_BYTES[i]]) {
                trace!("[SERIAL] handshake write error at byte {}: {}", i, e);
                ok = false;
                break;
            }
            let mut r = [0u8; 1];
            match transport.read_exact(&mut r) {
                Ok(_) => {
                    let b = r[0];
                    last_received.push(b);
                    if last_received.len() > 16 {
                        last_received.remove(0);
                    }
                    let expected = !SERIAL_HANDSHAKE_BYTES[i];
                    if b == expected {
                        mismatch = 0;
                        i += 1;
                    } else {
                        mismatch += 1;
                        residual.push(b);
                        if residual.len() > residual_window {
                            residual.remove(0);
                        }
                        // 残留DA流检测（A1/0B 模式）
                        if residual.len() >= residual_window
                            && residual.iter().all(|x| *x == 0xA1 || *x == 0x0B)
                        {
                            return Err(
                                "Preloader 握手读到疑似残留/错位响应流 (A1/0B)。请重新插拔或长按电源 10 秒，确认设备重新进入干净 Preloader 后再试。".to_string(),
                            );
                        }
                        if mismatch >= max_mismatch {
                            trace!(
                                "[SERIAL] handshake 连续 {} 次错位，重新开始本轮",
                                mismatch
                            );
                            ok = false;
                            break;
                        }
                        // 容忍错位：重置到字节 0 继续寻找真正的同步序列
                        i = 0;
                    }
                }
                Err(e) => {
                    trace!("[SERIAL] handshake read error at byte {}: {}", i, e);
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            trace!("SerialPort BROM 握手成功 (attempt {})", attempt + 1);
            return Ok(true);
        }
    }

    // 最终失败：回显最近收到的原始字节，便于定位（如 0x52='R' 表示设备未进入握手态）
    let hex: Vec<String> = last_received.iter().map(|b| format!("0x{:02X}", b)).collect();
    let diagnostic = if hex.is_empty() {
        "（未收到任何握手响应字节，可能是波特率不匹配或端口未就绪）".to_string()
    } else {
        format!("最近收到字节: {}", hex.join(" "))
    };
    Err(format!(
        "Preloader 握手失败：连续 {} 次尝试均未收到正确的同步响应。{}",
        max_attempts, diagnostic
    ))
}

impl BromTransport for SerialPortTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        let port = self.port.as_mut().ok_or("串口已关闭")?;
        port.write_all(data)
            .map_err(|e| format!("serial write: {}", e))?;
        Ok(data.len())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let port = self.port.as_mut().ok_or("串口已关闭")?;
        port.read_exact(buf)
            .map_err(|e| format!("serial read_exact: {}", e))?;
        Ok(buf.len())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let port = self.port.as_mut().ok_or("串口已关闭")?;
        port.read(buf).map_err(|e| format!("serial read: {}", e))
    }

    fn set_timeout(&mut self, duration: Duration) {
        self.timeout = duration;
        if let Some(port) = self.port.as_mut() {
            let _ = port.set_timeout(duration);
        }
    }

    fn get_timeout(&self) -> Duration {
        self.timeout
    }

    fn do_handshake(&mut self) -> Result<bool, String> {
        // BROM/Preloader 握手协议: 逐字节发送 [A0, 0A, 50, 05]，每字节期望取反回复。
        // 与 usb/device_handshake.rs 的 USB 握手保持一致的健壮性：
        // 多次重试 + 间隔 + 排空 + 错位计数重置(i=0) + 残留DA流检测。
        //
        // 设备进入握手态前可能持续发出杂散字节/文本（典型：期望 0x5F 却读到 0x52 'R'），
        // 故需容忍错位并在最终失败时回显原始字节，便于定位根因：
        //   - 连续 0x52('R') 等文本字节 → 设备未在握手态（可能阶段/驱动/DTR 问题）
        //   - 收到 0x5F 0x0F 0xA0 0x0A → 表明是 MTK 标准同步变体（非逐字节取反）
        const MAX_ATTEMPTS: u32 = 10;
        const RETRY_DELAY_MS: u64 = 300;
        const MAX_MISMATCH: u32 = 8;
        const RESIDUAL_WINDOW: usize = 4;
        const HANDSHAKE_TIMEOUT_MS: u64 = 500;

        // 缩短每字节响应超时，避免弱连接下 10 次重试累积成几十秒空等
        let orig_timeout = self.get_timeout();
        self.set_timeout(Duration::from_millis(HANDSHAKE_TIMEOUT_MS));
        let result = serial_do_handshake(self, MAX_ATTEMPTS, RETRY_DELAY_MS, MAX_MISMATCH, RESIDUAL_WINDOW);
        self.set_timeout(orig_timeout);
        result
    }

    fn is_libusb(&self) -> bool {
        false
    }

    /// 串口模式下排空残留数据：循环读取直到超时，防止 FORMAT 等命令后的残留干扰后续操作
    fn drain_pipes(&mut self) {
        let original_timeout = self.timeout;
        if let Some(port) = self.port.as_mut() {
            // 设置短超时（50ms），快速轮询排空
            let _ = port.set_timeout(Duration::from_millis(50));
            let mut buf = [0u8; 512];
            let mut total = 0usize;
            for _ in 0..20 {
                match port.read(&mut buf) {
                    Ok(n) if n > 0 => total += n,
                    _ => break,
                }
            }
            if total > 0 {
                trace!("[SERIAL] drain_pipes 排空 {} 字节残留数据", total);
            }
            // 恢复原始超时
            let _ = port.set_timeout(original_timeout);
        }
    }

    fn close_device(&mut self) -> Result<(), String> {
        // 关闭串口：drop port 即可释放 COM 口资源
        self.port.take();
        trace!("[SERIAL] 串口已关闭");
        Ok(())
    }
}

/// UsbDevice 实现 BROM 传输
impl BromTransport for USB设备 {
    fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        USB设备::写入(self, data)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        USB设备::精确读取(self, buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        USB设备::读取(self, buf)
    }

    fn submit_read_request(&mut self, len: usize) -> Result<bool, String> {
        USB设备::预提交读取(self, len)
    }

    fn complete_read_request(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        USB设备::完成预提交读取(self, buf)
    }

    fn read_exact_vec(&mut self, len: usize) -> Result<Vec<u8>, String> {
        USB设备::精确读取到Vec(self, len)
    }

    fn cancel_pending_transfers(&mut self) {
        USB设备::取消挂起传输(self)
    }

    fn drain_pending(&mut self) {
        USB设备::drain_pending(self)
    }

    fn drain_pipes(&mut self) {
        USB设备::drain_pipes(self)
    }

    fn set_timeout(&mut self, duration: Duration) {
        USB设备::设置超时(self, duration);
    }

    fn get_timeout(&self) -> Duration {
        USB设备::获取超时(self)
    }

    fn do_handshake(&mut self) -> Result<bool, String> {
        USB设备::执行握手(self)
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

    fn 获取输出端点最大包大小(&self) -> u16 {
        USB设备::获取输出端点最大包大小(self)
    }

    fn ctrl_transfer_out(
        &mut self,
        req_type: u8,
        req: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<usize, String> {
        USB设备::控制传输输出(self, req_type, req, value, index, data)?;
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
        USB设备::控制传输输入(self, req_type, req, value, index, length)
    }

    fn clear_halt_in(&mut self) -> Result<(), String> {
        USB设备::清除输入端点停顿(self)
    }

    fn clear_halt_out(&mut self) -> Result<(), String> {
        USB设备::清除输出端点停顿(self)
    }

    fn reset_device(&mut self) -> Result<(), String> {
        USB设备::reset_device(self)
    }

    fn close_device(&mut self) -> Result<(), String> {
        USB设备::关闭(self);
        Ok(())
    }

    fn reopen_device(&mut self, context: &crate::usb::USB上下文) -> Result<(), String> {
        USB设备::重新打开(self, context)
    }
}
