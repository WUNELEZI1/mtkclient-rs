use super::{
    BromPortResult, BromTransport, FIND_BROM_INTERVAL_MS, SERIAL_HANDSHAKE_BYTES,
    SERIAL_OPEN_TIMEOUT_MS,
};
use log::trace;
use std::fs::OpenOptions;
use std::time::Duration;

/// Verify whether the COM port really exists
///
/// serialport's available_ports() may return invalid ports left over in the registry;
/// open \\.\COMx via CreateFile to verify the port is actually usable.
pub(crate) fn verify_port_exists(port_name: &str) -> bool {
    let path = format!("\\\\.\\{}", port_name);
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .is_ok()
}

/// BROM transport implemented via serialport
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

    /// Open the serial port directly without verify_port_exists validation
    ///
    /// The Pattern protocol must use this path! verify_port_exists opens and then closes the COM port,
    /// wiping the READY signal sent by the Preloader from the serial receive buffer.
    ///
    /// Matches Python SerialClass.connect(): dsrdtr=False, rtscts=False
    pub fn open_raw(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        let mut port = serialport::new(port_name, baud_rate)
            .timeout(Duration::from_millis(SERIAL_OPEN_TIMEOUT_MS))
            .data_bits(serialport::DataBits::Eight)
            .stop_bits(serialport::StopBits::One)
            .parity(serialport::Parity::None)
            .flow_control(serialport::FlowControl::None)
            .open()
            .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;
        // Match Python: dsrdtr=False — disable the DTR signal
        // The DTR signal causes the device to enter BROM handshake mode
        let _ = port.write_data_terminal_ready(false);
        trace!(
            "[SERIAL] {} 已打开(raw, DTR=off): baud={}, 8N1, no_flow_control",
            port_name, baud_rate
        );
        // Brief delay after opening to let the device firmware/driver settle, reducing the race where
        // "the port appears and the handshake starts immediately" on weak connections (the device needs
        // tens of ms to stabilize after enumeration). Sleep only, without clearing the receive buffer,
        // so the Pattern protocol's READY signal is unaffected.
        std::thread::sleep(Duration::from_millis(150));
        Ok(SerialPortTransport {
            port: Some(port),
            timeout: Duration::from_millis(SERIAL_OPEN_TIMEOUT_MS),
        })
    }

    /// Enumerate all COM ports and find a MediaTek BROM/Preloader device
    /// Polls indefinitely, scanning every 200ms until found
    pub fn find_brom_port() -> Option<BromPortResult> {
        Self::find_brom_port_with_timeout(u64::MAX)
    }

    /// Enumerate all COM ports and find a MediaTek BROM/Preloader device
    /// Timeout variant; returns None on timeout
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
                    // BROM mode: VID=0E8D PID=0003
                    // Preloader mode: VID=0E8D PID=2000
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

/// Serial handshake core: send `A0,0A,50,05` byte by byte, expecting a bytewise inverted reply.
///
/// Matches the robustness of the USB handshake in usb/device_handshake.rs:
/// several retries + spacing + drain + mismatch counter reset (i=0) + residual-DA stream detection.
/// Before entering the handshake state the device may keep emitting stray bytes/text (typical: 0x5F
/// expected but 0x52 'R' read), so misalignment must be tolerated and, on final failure, the raw bytes
/// echoed for diagnosis:
///   - continuous text bytes like 0x52('R') → device not in handshake state (likely stage/driver/DTR issue)
///   - received 0x5F 0x0F 0xA0 0x0A → indicates the MTK standard sync variant (non bytewise inversion)
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

        // Drain leftovers only on retries; the first attempt must preserve the Preloader "READY" sync
        // signal in the receive buffer (as the device enters the handshake state it emits READY byte by
        // byte in step with the host; a successful flashing-box log shows 0x52/45/41/44/59 = "READY").
        // Draining on the first attempt would wipe these bytes and cause the subsequent handshake to
        // "receive no response bytes at all".
        if attempt > 0 {
            transport.drain_pipes();
        }

        let mut ok = true;
        let mut mismatch: u32 = 0;
        let mut residual: Vec<u8> = Vec::with_capacity(residual_window);
        last_received.clear();
        let mut i = 0usize;
        // Match flashing-box behavior: before the device enters the handshake state, writing A0 may get
        // no response temporarily (a Preloader device does not respond to A0 before emitting READY, so
        // A0 must be written repeatedly as a probe). Cap consecutive no-response during the "byte 0 sync
        // probe" phase to avoid an infinite loop; within the cap keep writing A0 until the device starts
        // emitting READY / inverted responses.
        const SYNC_PROBE_LIMIT: u32 = 25; // 25 no-responses = 25 * 500ms ≈ 12.5s (probe window before the device emits READY)
        let mut sync_probe = 0u32;
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
                        // Before its formal response, a Preloader device emits the "READY" (0x52/45/41/44/59)
                        // sync text byte by byte (flashing-box log: two full R E A D Y R E A D Y passes
                        // before the inverted handshake begins). These are normal device sync output and
                        // must not count toward mismatch (otherwise two READY passes = 10 bytes would exceed
                        // max_mismatch=8 and cause a false failure), nor trigger residual-DA stream
                        // detection. Note: the byte was already pushed into last_received above; no need to
                        // repeat it here.
                        if matches!(b, b'R' | b'E' | b'A' | b'D' | b'Y') {
                            trace!(
                                "[SERIAL] 读到 READY 同步文本字节 0x{:02X}('{}')，忽略",
                                b, b as char
                            );
                            continue;
                        }
                        mismatch += 1;
                        residual.push(b);
                        if residual.len() > residual_window {
                            residual.remove(0);
                        }
                        // Residual-DA stream detection (A1/0B pattern)
                        if residual.len() >= residual_window
                            && residual.iter().all(|x| *x == 0xA1 || *x == 0x0B)
                        {
                            return Err(
                                "Preloader 握手读到疑似残留/错位响应流 (A1/0B)。请重新插拔或长按电源 10 秒，确认设备重新进入干净 Preloader 后再试。".to_string(),
                            );
                        }
                        if mismatch >= max_mismatch {
                            trace!("[SERIAL] handshake 连续 {} 次错位，重新开始本轮", mismatch);
                            ok = false;
                            break;
                        }
                        // Tolerate misalignment: reset to byte 0 and keep looking for the real sync sequence
                        i = 0;
                    }
                }
                Err(e) => {
                    // Tolerate no-response only while "still in the byte 0 sync probe phase" (the device
                    // does not respond to A0 before emitting READY, so A0 must keep being written as a
                    // probe, matching the flashing box); only fail once the formal handshake has begun (i>0).
                    if i == 0 {
                        sync_probe += 1;
                        if sync_probe >= SYNC_PROBE_LIMIT {
                            trace!("[SERIAL] 同步试探 {} 次无响应，放弃本轮", sync_probe);
                            ok = false;
                            break;
                        }
                        trace!(
                            "[SERIAL] handshake 写 A0 无响应 (attempt {}, probe {})，继续试探...",
                            attempt + 1,
                            sync_probe
                        );
                        continue;
                    }
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

    // Final failure: echo the most recently received raw bytes for diagnosis (e.g. 0x52='R' means the device is not in handshake state)
    let hex: Vec<String> = last_received
        .iter()
        .map(|b| format!("0x{:02X}", b))
        .collect();
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
        super::trace_io("->", data);
        let port = self.port.as_mut().ok_or("串口已关闭")?;
        port.write_all(data)
            .map_err(|e| format!("serial write: {}", e))?;
        Ok(data.len())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let port = self.port.as_mut().ok_or("串口已关闭")?;
        port.read_exact(buf)
            .map_err(|e| format!("serial read_exact: {}", e))?;
        super::trace_io("<-", buf);
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
        // BROM/Preloader handshake protocol: send [A0, 0A, 50, 05] byte by byte, expecting an inverted reply per byte.
        // Matches the robustness of the USB handshake in usb/device_handshake.rs:
        // several retries + spacing + drain + mismatch counter reset (i=0) + residual-DA stream detection.
        //
        // Before entering the handshake state the device may keep emitting stray bytes/text (typical: 0x5F expected but 0x52 'R' read),
        // so misalignment must be tolerated and, on final failure, the raw bytes echoed for diagnosis:
        //   - continuous text bytes like 0x52('R') → device not in handshake state (likely stage/driver/DTR issue)
        //   - received 0x5F 0x0F 0xA0 0x0A → indicates the MTK standard sync variant (non bytewise inversion)
        const MAX_ATTEMPTS: u32 = 10;
        const RETRY_DELAY_MS: u64 = 300;
        const MAX_MISMATCH: u32 = 8;
        const RESIDUAL_WINDOW: usize = 4;
        const HANDSHAKE_TIMEOUT_MS: u64 = 500;

        // Shorten the per-byte response timeout to avoid 10 retries accumulating into tens of seconds of idle waiting on weak connections
        let orig_timeout = self.get_timeout();
        self.set_timeout(Duration::from_millis(HANDSHAKE_TIMEOUT_MS));
        let result = serial_do_handshake(
            self,
            MAX_ATTEMPTS,
            RETRY_DELAY_MS,
            MAX_MISMATCH,
            RESIDUAL_WINDOW,
        );
        self.set_timeout(orig_timeout);
        result
    }

    fn is_libusb(&self) -> bool {
        false
    }

    /// Drain leftover data in serial mode: read in a loop until timeout, preventing residue after commands like FORMAT from interfering with later operations
    fn drain_pipes(&mut self) {
        let original_timeout = self.timeout;
        if let Some(port) = self.port.as_mut() {
            // Use a short timeout (50ms) for fast polling drain
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
            // Restore the original timeout
            let _ = port.set_timeout(original_timeout);
        }
    }

    fn close_device(&mut self) -> Result<(), String> {
        // Close the serial port: dropping port releases the COM port resources
        self.port.take();
        trace!("[SERIAL] 串口已关闭");
        Ok(())
    }
}
