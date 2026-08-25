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
        if crate::cancel::requested() || crate::cancel::force_requested() {
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
        if crate::cancel::requested() || crate::cancel::force_requested() {
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
    //
    // 关键修正：设备的回传确认（如 FASTBOOT 的 "TOOBTSAF"）可能被拆成多个 USB/串口事务分片到达
    // （例如首次读得 "TOOBT"、二次读得 "SAF"）。原先对每个独立 packet 做 windows() 匹配，8 字节的
    // TOOBTSAF 永远无法在拆片中匹配，于是降级为误导性的"收到非预期回传: 53 41 46" + "等待回传确认超时"
    // （设备其实已重启成功）。改为把多次读取累积到同一缓冲区，再在累积缓冲上做窗口匹配。
    let mut acc: Vec<u8> = Vec::new();
    let resp_deadline = std::time::Instant::now();
    let resp_max_wait = Duration::from_secs(3);

    loop {
        if crate::cancel::requested() || crate::cancel::force_requested() {
            return Err("用户取消".to_string());
        }
        if resp_deadline.elapsed() > resp_max_wait {
            info!("[BootAs] 等待回传确认超时，设备可能已重启");
            break;
        }
        let chunk = match read_packet(device, Duration::from_millis(1000)) {
            Ok(r) => r,
            Err(_) => {
                info!("[BootAs] 读取中断（设备可能已重启）");
                break;
            }
        };

        if chunk.is_empty() {
            continue;
        }
        acc.extend_from_slice(&chunk);

        // 在累积缓冲区上检查预期的回传确认（目标标识可能和其他数据混排在一起）
        let matched = mode
            .response_ids()
            .iter()
            .any(|id| acc.windows(id.len()).any(|w| w == *id));

        if matched {
            info!("[BootAs] 收到回传确认: {} ✓", String::from_utf8_lossy(&acc));

            // ATEMATEM 流程处理（在累积缓冲上做子串匹配，避免拆片漏判）
            if acc.windows(8).any(|w| w == b"ATEM0001") {
                device
                    .write(&[
                        0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xC0,
                    ])
                    .ok();
            } else if acc.windows(8).any(|w| w == b"ATEM0002") {
                device
                    .write(&[
                        0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xC0,
                        0x00, 0x80, 0x00, 0x00,
                    ])
                    .ok();
            } else if acc.windows(8).any(|w| w == b"ATEMATEX") {
                info!("[BootAs] ATEMATEM 握手完成");
            }
            break;
        }

        // 未匹配时仅 trace，避免把拆片回传刷成噪音（设备已重启常见的非完整分片）
        log::trace!("[BootAs] 累积缓冲（未匹配）: {}", hex_str(&acc));
    }

    // Step 4: 发送 DISCONNECT（设备可能已重启，失败忽略）
    info!("[BootAs] 发送 DISCONNECT");
    let _ = device.write(DISCONNECT_CMD);

    device.set_timeout(orig_timeout);

    info!("[BootAs] reboot to {}...", mode.name().to_lowercase());
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

/// 枚举所有 Preloader COM 口（不使用 verify_port_exists，避免清空串口缓冲区）
///
/// 直接通过 serialport::available_ports() 枚举，返回所有 VID=0E8D PID=2000 的串口名。
/// 不调用 verify_port_exists()，因为它会用 CreateFile 打开再关闭 COM 口，清空缓冲区中的 READY。
///
/// 重要：Windows 注册表可能残留历史 COM 口条目（设备已拔出/重枚举但条目未清除），
/// available_ports() 会把这些“幽灵端口”也枚举出来。因此返回**全部**候选而非首个，
/// 由调用方对每个候选尝试 open_raw，打开失败即视为幽灵端口，跳过并继续尝试其余候选，
/// 绝不能像旧实现那样“首个端口打开失败就退出轮询”（这正是 reboot --via preloader 偶发失败的元凶）。
pub(crate) fn scan_preloader_ports() -> Vec<String> {
    let ports = match serialport::available_ports() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let mut result = Vec::new();
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type {
            if info.vid == 0x0E8D && info.pid == 0x2000 {
                result.push(p.port_name.clone());
            }
        }
    }
    result
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
        if crate::cancel::requested() || crate::cancel::force_requested() {
            return Err("用户取消".to_string());
        }
        if deadline.elapsed() > max_wait {
            return Err("未在 15 秒内找到可用的 Preloader COM 口".into());
        }

        // 枚举当前所有 Preloader VCOM（可能含 Windows 注册表残留的幽灵端口）。
        // 对每个候选尝试打开：幽灵端口（注册表残留/设备尚未就绪）打开会失败，
        // 此时跳过并继续尝试其余候选，而不是像旧实现那样直接退出轮询。
        let candidates = scan_preloader_ports();
        for port_name in &candidates {
            let mut transport = match SerialPortTransport::open_raw(port_name, 115200) {
                Ok(t) => t,
                Err(e) => {
                    debug!(
                        "串口 {} 打开失败（可能为注册表残留或设备未就绪），跳过: {}",
                        port_name, e
                    );
                    continue;
                }
            };
            debug!("发现并打开 Preloader COM 口: {}", port_name);
            // 打开成功 → 执行 Pattern 协议（其内部已处理设备已重启导致的写/读失败）。
            // 无论成功与否都按语义返回，不再因幽灵端口而提前退出。
            transport.set_timeout(Duration::from_millis(2000));
            return send_boot_pattern(&mut transport, boot_mode);
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}
