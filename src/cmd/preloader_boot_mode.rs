//! Preloader-mode BootMode switch protocol (Layer 1).
//!
//! Mirrors Python mtkclient's `META.init()`.
//!
//! Protocol flow (CDC bulk transfer):
//! 1. Open the Preloader VCOM serial port (PID=0x2000), **without any BROM handshake**
//! 2. Keep reading until "READY" arrives
//! 3. Send the 8-byte mode identifier (e.g. FASTBOOT)
//! 4. Keep reading until a non-READY response arrives (e.g. TOOBTSAF)
//! 5. Send DISCONNECT
//!
//! Key point: the pattern protocol must run BEFORE any BROM handshake!
//! Python mtkclient META.init() drives the CDC USB endpoint directly and never handshakes.
//! A prior BROM handshake puts the device into BROM protocol state, which echoes pattern data
//! back as BROM commands.
//!
//! Also key: when scanning ports do not use verify_port_exists() (it opens and closes the COM
//! port, draining the buffered READY); instead enumerate to PID=0x2000 and open immediately
//! with SerialPortTransport::new().

use std::time::Duration;

use log::{debug, info, warn};

use crate::preloader::transport::BromTransport;

/// DISCONNECT command
const DISCONNECT_CMD: &[u8] = b"DISCONNECT";

/// Supported boot modes
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

/// Read one Preloader data fragment.
///
/// Matching Python: bytearray(ep_in(maxinsize)) — CDC bulk returns one USB transaction at a time.
/// A serial port is a continuous stream and may deliver several logical messages at once
/// (e.g. "READYREADYREADY..."). A small buffer (16 bytes) plus a short timeout approximates
/// CDC bulk packet boundaries.
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
        // Use a short timeout (50ms) per read to approximate CDC bulk per-packet behavior
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

/// Switch boot mode through the Preloader pattern protocol.
///
/// Mirrors Python mtkclient META.init():
/// - keep reading until "READY"
/// - send the 8-byte mode identifier
/// - keep reading until an acknowledgement arrives (non-READY)
/// - send DISCONNECT
pub fn send_boot_pattern(device: &mut dyn BromTransport, mode: BootMode) -> Result<(), String> {
    info!("[BootAs] Start Pattern protocol... mode={}", mode.name());

    let orig_timeout = device.get_timeout();

    // Step 1: Keep reading until "READY" arrives, then send the mode identifier at once.
    // Matching Python: if resp == b"READY": ep_out(metamode, len(metamode))
    // Key: send immediately after READY; never delay — the Preloader READY window is short.
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

        // Check for READY (a serial stream may deliver "READYREADYREADY...")
        if resp.windows(5).any(|w| w == b"READY") {
            info!("[BootAs] Receive READY");
            ready_received = true;

            // Send the mode identifier as soon as READY arrives (matching Python: ep_out(metamode, len(metamode)))
            let send_id = mode.send_id();
            info!(
                "[BootAs] 发送模式标识: {} ({})",
                mode.name(),
                hex_str(send_id)
            );
            // The device may reboot right after the identifier (COM port drops); treat a write failure as success
            if let Err(e) = device.write(send_id) {
                warn!("[BootAs] 发送模式标识后设备可能已重启: {}", e);
                device.set_timeout(orig_timeout);
                return Ok(());
            }
            break; // sent; leave the loop
        }

        if resp.is_empty() {
            continue;
        }

        log::trace!("[BootAs] 收到数据（非 READY）: {}", hex_str(&resp));
    }

    if !ready_received {
        warn!("[BootAs] 未收到 READY，设备可能已处于就绪状态");
        // Try sending even if READY was never received
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

    // Step 2: Read the acknowledgement (the device may reboot immediately; read errors count as success).
    //
    // Key fix: the acknowledgement (e.g. "TOOBTSAF" for FASTBOOT) may arrive split across several
    // USB/serial transactions (first read "TOOBT", second read "SAF"). Matching windows() within each
    // individual packet could never match the 8-byte TOOBTSAF, degrading into the misleading
    // "unexpected response: 53 41 46" plus "acknowledgement timeout" (although the device had in fact
    // rebooted successfully). Accumulate reads into one buffer and window-match on the accumulated buffer.
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

        // Check the expected acknowledgement on the accumulated buffer (the target ID may be interleaved with other data)
        let matched = mode
            .response_ids()
            .iter()
            .any(|id| acc.windows(id.len()).any(|w| w == *id));

        if matched {
            info!("[BootAs] 收到回传确认: {} ✓", String::from_utf8_lossy(&acc));

            // ATEMATEM flow (substring match on the accumulated buffer to avoid split-chunk misses)
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

        // Only trace on mismatch to avoid noisy logs from split fragments (common after a device reboot)
        log::trace!("[BootAs] 累积缓冲（未匹配）: {}", hex_str(&acc));
    }

    // Step 4: Send DISCONNECT (the device may already have rebooted; ignore failures)
    info!("[BootAs] 发送 DISCONNECT");
    let _ = device.write(DISCONNECT_CMD);

    device.set_timeout(orig_timeout);

    info!("[BootAs] reboot to {}...", mode.name().to_lowercase());
    std::thread::sleep(Duration::from_millis(500));

    Ok(())
}

/// Enumerate all Preloader COM ports (no verify_port_exists, which would drain the serial buffer).
///
/// Enumerates directly via serialport::available_ports() and returns every port with
/// VID=0E8D PID=2000. verify_port_exists() is not used because it opens and closes the COM port
/// with CreateFile, draining the buffered READY.
///
/// Important: the Windows registry may keep stale COM port entries (device unplugged or
/// re-enumerated but the entry not cleared), and available_ports() also enumerates these
/// "ghost ports". So return **all** candidates instead of the first one, and let the caller try
/// open_raw on each: a failed open means a ghost port, which is skipped while the remaining
/// candidates are still tried. Never exit the polling loop on the first open failure as the old
/// implementation did (that was the cause of intermittent reboot --via preloader failures).
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

/// Try to connect to the Preloader serial port and run the pattern protocol.
///
/// Waits for the Preloader VCOM (PID=0x2000), opens the raw serial port (no handshake) and runs
/// the pattern protocol.
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

        // Enumerate all current Preloader VCOMs (may include ghost ports left in the Windows registry).
        // Try to open each candidate: ghost ports (registry leftovers / device not ready) fail to open,
        // in which case skip them and try the rest instead of leaving the polling loop like the old code.
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
            // Opened successfully → run the pattern protocol (it already handles write/read failures
            // caused by a device reboot). Return the result as-is; ghost ports no longer end the loop early.
            transport.set_timeout(Duration::from_millis(2000));
            return send_boot_pattern(&mut transport, boot_mode);
        }

        std::thread::sleep(Duration::from_millis(200));
    }
}

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}
