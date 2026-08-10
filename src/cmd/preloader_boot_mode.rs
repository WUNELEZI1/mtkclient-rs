//! Preloader 模式 BootMode Switch 协议 (Layer 1)
//!
//! 对齐 Python mtkclient `META.init()` 的实现。
//!
//! 协议流程（CDC bulk transfer）：
//! 1. 打开 Preloader VCOM 串口 (PID=0x2000)，**不做 BROM 握手**
//! 2. 持续读取直到收到 "READY"
//! 3. 发送 8 字节模式标识（如 FASTBOOT）
//! 4. 持续读取直到收到非 READY 的响应（如 TOOBTSAF）
//! 5. 发送 DISCONNECT
//!
//! 关键：Pattern 协议必须在 BROM 握手之前执行！
//! Python mtkclient META.init() 直接操作 CDC USB endpoint，完全不做 handshake。
//! 如果先做 BROM 握手，设备进入 BROM 协议状态，会把 Pattern 数据当作 BROM 命令回显。
//!
//! 另一个关键：扫描端口时不能用 verify_port_exists()（会打开再关闭 COM 口，清空缓冲区中的 READY），
//! 必须直接枚举到 PID=0x2000 后立即用 SerialPortTransport::new() 打开。

use std::time::Duration;

use log::{debug, info, warn};

use crate::preloader::transport::BromTransport;

/// DISCONNECT 命令
const DISCONNECT_CMD: &[u8] = b"DISCONNECT";

/// 支持的启动模式
#[derive(Debug, Clone, Copy)]
pub enum BootMode {
    Fastboot,
    Meta,
}

impl BootMode {
    fn send_id(&self) -> &'static [u8] {
        match self {
            BootMode::Fastboot => b"FASTBOOT",
            BootMode::Meta => b"METAMETA",
        }
    }

    fn response_ids(&self) -> &'static [&'static [u8]] {
        match self {
            BootMode::Fastboot => &[b"TOOBTSAF"],
            BootMode::Meta => &[b"ATEMATEM", b"METASLA"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            BootMode::Fastboot => "FASTBOOT",
            BootMode::Meta => "META",
        }
    }
}

/// 读取一个 Preloader 数据片段
///
/// 对齐 Python: bytearray(ep_in(maxinsize)) — CDC bulk 每次返回一个 USB 事务的数据。
/// 串口是连续流，可能一次性返回多个逻辑消息（如 "READYREADYREADY..."）。
/// 使用小 buffer（16 字节）+ 短超时，更接近 CDC bulk 的包边界行为。
fn read_packet(device: &mut dyn BromTransport, timeout: Duration) -> Result<Vec<u8>, String> {
    device.set_timeout(timeout);
    let mut buf = vec![0u8; 16];
    let mut total = 0;
    let deadline = std::time::Instant::now();

    loop {
        if crate::cancel::requested() {
            return Err("用户取消".to_string());
        }
        let remaining = timeout.saturating_sub(deadline.elapsed());
        if remaining.is_zero() {
            break;
        }
        // 每次读取使用短超时（50ms），更接近 CDC bulk 的逐包行为
        let read_timeout = std::cmp::min(Duration::from_millis(50), remaining);
        device.set_timeout(read_timeout);
        match device.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total >= 16 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf.truncate(total);
    Ok(buf)
}

/// 通过 Preloader Pattern 协议切换启动模式
///
/// 对齐 Python mtkclient META.init():
/// - 持续读取直到收到 "READY"
/// - 发送 8 字节模式标识
/// - 持续读取直到收到回传确认（非 READY）
/// - 发送 DISCONNECT
pub fn send_boot_pattern(device: &mut dyn BromTransport, mode: BootMode) -> Result<(), String> {
    info!("[BootAs] Start Pattern protocol... mode={}", mode.name());

    let orig_timeout = device.get_timeout();

    // Step 1: 持续读取直到收到 "READY"，收到后立即发送模式标识
    // 对齐 Python: if resp == b"READY": ep_out(metamode, len(metamode))
    // 关键：收到 READY 后必须立即发送，不能等待！Preloader 的 READY 窗口很短。
    let mut ready_received = false;
    let deadline = std::time::Instant::now();
    let max_wait = Duration::from_secs(10);

    loop {
        if crate::cancel::requested() {
            return Err("用户取消".to_string());
        }
        if deadline.elapsed() > max_wait {
            break;
        }
        let resp = match read_packet(device, Duration::from_millis(2000)) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // 检查是否包含 READY（串口模式下可能是 "READYREADYREADY..." 连续流）
        if resp.windows(5).any(|w| w == b"READY") {
            info!("[BootAs] Receive READY");
            ready_received = true;

            // 收到 READY 后立即发送模式标识（对齐 Python: ep_out(metamode, len(metamode))）
            let send_id = mode.send_id();
            info!(
                "[BootAs] 发送模式标识: {} ({})",
                mode.name(),
                hex_str(send_id)
            );
            // 发送模式标识后设备可能立即重启（COM 口断开），write 失败视为成功
            if let Err(e) = device.write(send_id) {
                warn!("[BootAs] 发送模式标识后设备可能已重启: {}", e);
                device.set_timeout(orig_timeout);
                return Ok(());
            }
            break; // 发送完成，跳出循环
        }

        if resp.is_empty() {
            continue;
        }

        log::trace!("[BootAs] 收到数据（非 READY）: {}", hex_str(&resp));
    }

    if !ready_received {
        warn!("[BootAs] 未收到 READY，设备可能已处于就绪状态");
        // 即使没收到 READY 也尝试发送
        let send_id = mode.send_id();
        info!(
            "[BootAs] 尝试发送模式标识: {} ({})",
            mode.name(),
            hex_str(send_id)
        );
        if let Err(e) = device.write(send_id) {
            warn!("[BootAs] 发送失败: {}", e);
            device.set_timeout(orig_timeout);
            return Ok(());
        }
    }

    // Step 2: 读取回传确认（设备可能立即重启，读错误视为成功）
    let resp_deadline = std::time::Instant::now();
    let resp_max_wait = Duration::from_secs(3);

    loop {
        if crate::cancel::requested() {
            return Err("用户取消".to_string());
        }
        if resp_deadline.elapsed() > resp_max_wait {
            info!("[BootAs] 等待回传确认超时，设备可能已重启");
            break;
        }
        let resp = match read_packet(device, Duration::from_millis(1000)) {
            Ok(r) => r,
            Err(_) => {
                info!("[BootAs] 读取中断（设备可能已重启）");
                break;
            }
        };

        // 模式标识发送后可能继续收到 READY，跳过
        if resp.windows(5).any(|w| w == b"READY") {
            log::trace!("[BootAs] 收到 READY（模式标识发送后），继续等待...");
            continue;
        }

        if resp.is_empty() {
            continue;
        }

        // 检查是否包含预期的回传确认（串口模式下可能和其他数据混在一起）
        let matched = mode
            .response_ids()
            .iter()
            .any(|id| resp.windows(id.len()).any(|w| w == *id));

        if matched {
            info!(
                "[BootAs] 收到回传确认: {} ✓",
                String::from_utf8_lossy(&resp)
            );

            // ATEMATEM 流程处理
            if resp == b"ATEM0001" {
                device
                    .write(&[
                        0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xC0,
                    ])
                    .ok();
                continue;
            } else if resp == b"ATEM0002" {
                device
                    .write(&[
                        0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xC0,
                        0x00, 0x80, 0x00, 0x00,
                    ])
                    .ok();
                continue;
            } else if resp == b"ATEMATEX" {
                info!("[BootAs] ATEMATEM 握手完成");
            }
            break;
        }

        warn!("[BootAs] 收到非预期回传: {} (继续等待...)", hex_str(&resp));
    }

    // Step 4: 发送 DISCONNECT（设备可能已重启，失败忽略）
    info!("[BootAs] 发送 DISCONNECT");
    let _ = device.write(DISCONNECT_CMD);

    device.set_timeout(orig_timeout);

    info!("[BootAs] reboot to {}...", mode.name().to_lowercase());
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

/// 扫描 Preloader COM 口（不使用 verify_port_exists，避免清空串口缓冲区）
///
/// 直接通过 serialport::available_ports() 枚举，找到 VID=0E8D PID=2000 的串口后立即返回。
/// 不调用 verify_port_exists()，因为它会用 CreateFile 打开再关闭 COM 口，清空缓冲区中的 READY。
pub(crate) fn scan_preloader_port() -> Option<String> {
    let ports = match serialport::available_ports() {
        Ok(p) => p,
        Err(_) => return None,
    };

    for p in &ports {
        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
            if info.vid == 0x0E8D && info.pid == 0x2000 {
                return Some(p.port_name.clone());
            }
        }
    }
    None
}

/// 尝试连接 Preloader 串口并发送 Pattern 协议
///
/// 等待 Preloader VCOM (PID=0x2000) 出现，打开原始串口（不握手），执行 Pattern 协议。
pub(crate) fn try_preloader_pattern(boot_mode: BootMode) -> Result<(), String> {
    use crate::preloader::SerialPortTransport;
    use std::time::Duration;

    info!("等待 Preloader COM 口出现（最多 15 秒）...");

    let deadline = std::time::Instant::now();
    let max_wait = Duration::from_secs(15);

    loop {
        if crate::cancel::requested() {
            return Err("用户取消".to_string());
        }
        if deadline.elapsed() > max_wait {
            return Err("未在 15 秒内找到 Preloader COM 口".into());
        }

        // 使用 scan_preloader_port（不做 verify，避免清空缓冲区中的 READY）
        if let Some(port_name) = scan_preloader_port() {
            debug!("发现 Preloader COM 口: {}", port_name);
            // 打开原始串口 — 使用 open_raw，不做 verify_port_exists！
            let mut transport = SerialPortTransport::open_raw(&port_name, 115200)
                .map_err(|e| format!("打开串口失败: {}", e))?;
            transport.set_timeout(Duration::from_millis(2000));

            send_boot_pattern(&mut transport, boot_mode)?;
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}
