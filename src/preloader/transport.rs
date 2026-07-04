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
            .timeout(Duration::from_millis(SERIAL_OPEN_TIMEOUT_MS))
            .open()
            .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;
        Ok(SerialPortTransport {
            port,
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
        for (i, &cmd) in SERIAL_HANDSHAKE_BYTES.iter().enumerate() {
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
        trace!("SerialPort BROM 握手成功");
        Ok(true)
    }

    fn is_libusb(&self) -> bool {
        false
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

    fn clear_halt_ep(&mut self, ep: u8) -> Result<(), String> {
        if ep & 0x80 != 0 {
            USB设备::清除输入端点停顿(self)
        } else {
            USB设备::清除输出端点停顿(self)
        }
    }
}
