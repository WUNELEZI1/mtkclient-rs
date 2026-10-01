//! USB设备::执行握手 — BROM 握手协议
//!
//! 协议：逐字节发送 [A0, 0A, 50, 05]。
//! 兼容两种已知的 MTK 响应模式（不同传输介质/芯片存在差异）：
//! - 取反模式（UART/串口典型）：设备返回每个发送字节的按位取反，如发 A0 → 收 5F。
//! - 回显模式（USB bulk 典型）：设备原样回显发送的字节，如发 A0 → 收 A0。
//!
//! 匹配时同时容忍两者，任一命中即推进，避免 "expected 0x5F got 0xA0" 的介质差异误判。
//!
//! Python 风格：失败计数器重置，最多 10 次重试，每次间隔 300ms。

use super::device::UsbDevice;
use super::log::usb_trace;
use log::info;
use std::time::Duration;

const HANDSHAKE_BYTE_SEQ: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];
const HANDSHAKE_MAX_ATTEMPTS: u32 = 10;
const HANDSHAKE_RETRY_DELAY_MS: u64 = 300;
const PRE_HANDSHAKE_A0_DELAY_MS: u64 = 10;
const HANDSHAKE_DRAIN_TIMEOUT_MS: u64 = 50;
const HANDSHAKE_MAX_MISMATCH: u32 = 8;
const RESIDUAL_DA_WINDOW: usize = 4;

fn handshake_should_restart_after_mismatch(mismatch_count: u32) -> bool {
    mismatch_count >= HANDSHAKE_MAX_MISMATCH
}

fn looks_like_da_residual_stream(bytes: &[u8]) -> bool {
    bytes.len() >= RESIDUAL_DA_WINDOW && bytes.iter().all(|b| matches!(*b, 0xA1 | 0x0B))
}

impl UsbDevice {
    pub fn do_handshake(&mut self) -> Result<bool, String> {
        // 使用缓存的设备类型判断握手策略
        if !self.device_type.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.write(&[0xA0]);
            std::thread::sleep(Duration::from_millis(PRE_HANDSHAKE_A0_DELAY_MS));
        }

        // Python retries handshakes with delays between attempts
        for attempt_count in 0..HANDSHAKE_MAX_ATTEMPTS {
            // 用户取消（Ctrl+C）时立即退出，避免跑完所有重试才停止
            if crate::cancel::force_requested() || crate::cancel::requested() {
                return Err("握手已取消".to_string());
            }
            if attempt_count > 0 {
                info!(
                    "[USB] handshake attempt {}/{}, waiting {}ms...",
                    attempt_count + 1,
                    HANDSHAKE_MAX_ATTEMPTS,
                    HANDSHAKE_RETRY_DELAY_MS
                );
                std::thread::sleep(Duration::from_millis(HANDSHAKE_RETRY_DELAY_MS));
            }
            // Drain any stale data first
            let orig_timeout = self.timeout;
            self.in_buf.clear();
            self.timeout = Duration::from_millis(HANDSHAKE_DRAIN_TIMEOUT_MS);
            let mut drain_buf = vec![0u8; self.in_ep_max_packet_size as usize];
            loop {
                match self.read(&mut drain_buf) {
                    Ok(n) if n > 0 => continue,
                    _ => break,
                }
            }
            self.timeout = orig_timeout;

            let mut success = true;
            let mut mismatch_count = 0;
            let mut last_mismatch_byte: Vec<u8> = Vec::with_capacity(RESIDUAL_DA_WINDOW);
            let mut i = 0;
            while i < HANDSHAKE_BYTE_SEQ.len() {
                if let Err(e) = self.write(&[HANDSHAKE_BYTE_SEQ[i]]) {
                    info!("[USB] handshake write error at byte {}: {}", i, e);
                    success = false;
                    break;
                }
                // echo write trace
                usb_trace(
                    "TX",
                    "USB设备::执行握手 echo_write",
                    &[HANDSHAKE_BYTE_SEQ[i]],
                );
                let mut r = [0u8; 1]; // 握手 echo 每次只读 1 字节
                match self.read(&mut r) {
                    Ok(n) if n > 0 => {
                        // echo read trace
                        usb_trace("RX", "USB设备::执行握手 echo_read", &r[..n]);
                        let last_byte = r[n - 1];
                        // 兼容取反（!发送字节，如 A0→5F）与回显（发送字节原样，如 A0→A0）两种
                        // MTK 响应模式：不同传输介质/设备存在差异，任一命中即推进。
                        if last_byte == !HANDSHAKE_BYTE_SEQ[i] || last_byte == HANDSHAKE_BYTE_SEQ[i]
                        {
                            mismatch_count = 0;
                            i += 1;
                        } else {
                            mismatch_count += 1;
                            last_mismatch_byte.push(last_byte);
                            if last_mismatch_byte.len() > RESIDUAL_DA_WINDOW {
                                last_mismatch_byte.remove(0);
                            }
                            if mismatch_count <= 3
                                || handshake_should_restart_after_mismatch(mismatch_count)
                            {
                                info!(
                                    "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                                    i, last_byte, !HANDSHAKE_BYTE_SEQ[i]
                                );
                            }
                            if looks_like_da_residual_stream(&last_mismatch_byte) {
                                return Err(
                                    "BROM 握手读到疑似残留/错位响应流 (A1/0B)。请重新插拔或长按电源 10 秒，确认设备重新进入干净 BROM 后再试。"
                                        .to_string(),
                                );
                            }
                            if handshake_should_restart_after_mismatch(mismatch_count) {
                                info!(
                                    "[USB] handshake mismatch 连续 {} 次，重新排空并开始下一轮",
                                    mismatch_count
                                );
                                success = false;
                                break;
                            }
                            i = 0; // Python 重置计数器
                        }
                    }
                    _ => {
                        info!("[USB] handshake read error at byte {}", i);
                        success = false;
                        break;
                    }
                }
            }
            if success {
                info!("Handshake OK");
                return Ok(true);
            }
        }
        Err(crate::error::ProtocolError::HandshakeFailed(HANDSHAKE_MAX_ATTEMPTS).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_restarts_after_too_many_mismatches() {
        assert!(!handshake_should_restart_after_mismatch(
            HANDSHAKE_MAX_MISMATCH - 1
        ));
        assert!(handshake_should_restart_after_mismatch(
            HANDSHAKE_MAX_MISMATCH
        ));
    }

    #[test]
    fn handshake_detects_da_residual_stream_pattern() {
        assert!(looks_like_da_residual_stream(&[0xA1, 0x0B, 0xA1, 0x0B]));
        assert!(!looks_like_da_residual_stream(&[0xA1, 0x0B, 0x5F, 0xF5]));
        assert!(!looks_like_da_residual_stream(&[0xA1, 0x0B, 0xA1]));
    }
}
