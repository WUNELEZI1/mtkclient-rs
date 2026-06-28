//! UsbDevice::do_handshake — BROM 握手协议
//!
//! 协议：逐字节发送 [A0, 0A, 50, 05]，每字节期望取反回复。
//! Python 风格：失败计数器重置，最多 10 次重试，每次间隔 300ms。

use super::device::UsbDevice;
use super::log::usb_trace;
use log::info;
use std::time::Duration;

const HANDSHAKE_BYTES: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];
const HANDSHAKE_MAX_ATTEMPT: u32 = 10;
const HANDSHAKE_RETRY_DELAY_MS: u64 = 300;
const HANDSHAKE_PRE_SEND_A0_DELAY_MS: u64 = 10;
const HANDSHAKE_DRAIN_TIMEOUT_MS: u64 = 50;

impl UsbDevice {
    pub fn do_handshake(&mut self) -> Result<bool, String> {
        // 使用缓存的设备类型判断握手策略
        if !self.device_type.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.write(&[0xA0]);
            std::thread::sleep(Duration::from_millis(HANDSHAKE_PRE_SEND_A0_DELAY_MS));
        }

        // Python retries handshakes with delays between attempts
        for attempt in 0..HANDSHAKE_MAX_ATTEMPT {
            if attempt > 0 {
                info!(
                    "[USB] handshake attempt {}/{}, waiting {}ms...",
                    attempt + 1,
                    HANDSHAKE_MAX_ATTEMPT,
                    HANDSHAKE_RETRY_DELAY_MS
                );
                std::thread::sleep(Duration::from_millis(HANDSHAKE_RETRY_DELAY_MS));
            }
            // Drain any stale data first
            let orig_timeout = self.timeout;
            self.timeout = Duration::from_millis(HANDSHAKE_DRAIN_TIMEOUT_MS);
            let mut drain = vec![0u8; self.ep_in_max_packet_size as usize];
            loop {
                match self.read(&mut drain) {
                    Ok(n) if n > 0 => continue,
                    _ => break,
                }
            }
            self.timeout = orig_timeout;

            let mut ok = true;
            let mut i = 0;
            while i < HANDSHAKE_BYTES.len() {
                if let Err(e) = self.write(&[HANDSHAKE_BYTES[i]]) {
                    info!("[USB] handshake write error at byte {}: {}", i, e);
                    ok = false;
                    break;
                }
                // echo write trace
                usb_trace(
                    "TX",
                    "UsbDevice::do_handshake echo_write",
                    &[HANDSHAKE_BYTES[i]],
                );
                let mut r = [0u8; 1]; // 握手 echo 每次只读 1 字节
                match self.read(&mut r) {
                    Ok(n) if n > 0 => {
                        // echo read trace
                        usb_trace("RX", "UsbDevice::do_handshake echo_read", &r[..n]);
                        let last_byte = r[n - 1];
                        if last_byte == !HANDSHAKE_BYTES[i] {
                            i += 1;
                        } else {
                            info!(
                                "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                                i, last_byte, !HANDSHAKE_BYTES[i]
                            );
                            i = 0; // Python 重置计数器
                        }
                    }
                    _ => {
                        info!("[USB] handshake read error at byte {}", i);
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                info!("Handshake OK");
                return Ok(true);
            }
        }
        Err(format!(
            "Handshake failed after {} attempts",
            HANDSHAKE_MAX_ATTEMPT
        ))
    }
}
