//! USB设备::执行握手 — BROM 握手协议
//!
//! 协议：逐字节发送 [A0, 0A, 50, 05]，每字节期望取反回复。
//! Python 风格：失败计数器重置，最多 10 次重试，每次间隔 300ms。

use super::device::USB设备;
use super::log::usb_trace;
use log::info;
use std::time::Duration;

const 握手字节序列: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];
const 握手最大尝试次数: u32 = 10;
const 握手重试延迟毫秒: u64 = 300;
const 握手前发送A0延迟毫秒: u64 = 10;
const 握手排空超时毫秒: u64 = 50;
const 单次握手最大错位次数: u32 = 8;
const 残留DA流检测窗口: usize = 4;

fn handshake_should_restart_after_mismatch(mismatch_count: u32) -> bool {
    mismatch_count >= 单次握手最大错位次数
}

fn looks_like_da_residual_stream(bytes: &[u8]) -> bool {
    bytes.len() >= 残留DA流检测窗口 && bytes.iter().all(|b| matches!(*b, 0xA1 | 0x0B))
}

impl USB设备 {
    pub fn 执行握手(&mut self) -> Result<bool, String> {
        // 使用缓存的设备类型判断握手策略
        if !self.设备类型.is_brom() {
            info!(
                "[USB] non-BROM PID (0x{:04X}), sending 0xA0 first",
                self.pid
            );
            let _ = self.写入(&[0xA0]);
            std::thread::sleep(Duration::from_millis(握手前发送A0延迟毫秒));
        }

        // Python retries handshakes with delays between attempts
        for 尝试次数 in 0..握手最大尝试次数 {
            if 尝试次数 > 0 {
                info!(
                    "[USB] handshake attempt {}/{}, waiting {}ms...",
                    尝试次数 + 1,
                    握手最大尝试次数,
                    握手重试延迟毫秒
                );
                std::thread::sleep(Duration::from_millis(握手重试延迟毫秒));
            }
            // Drain any stale data first
            let 原始超时 = self.超时;
            self.输入暂存.clear();
            self.超时 = Duration::from_millis(握手排空超时毫秒);
            let mut 排空缓冲区 = vec![0u8; self.输入端点最大包大小 as usize];
            loop {
                match self.读取(&mut 排空缓冲区) {
                    Ok(n) if n > 0 => continue,
                    _ => break,
                }
            }
            self.超时 = 原始超时;

            let mut 成功 = true;
            let mut 错位次数 = 0;
            let mut 最近错位字节: Vec<u8> = Vec::with_capacity(残留DA流检测窗口);
            let mut i = 0;
            while i < 握手字节序列.len() {
                if let Err(e) = self.写入(&[握手字节序列[i]]) {
                    info!("[USB] handshake write error at byte {}: {}", i, e);
                    成功 = false;
                    break;
                }
                // echo write trace
                usb_trace("TX", "USB设备::执行握手 echo_write", &[握手字节序列[i]]);
                let mut r = [0u8; 1]; // 握手 echo 每次只读 1 字节
                match self.读取(&mut r) {
                    Ok(n) if n > 0 => {
                        // echo read trace
                        usb_trace("RX", "USB设备::执行握手 echo_read", &r[..n]);
                        let 最后字节 = r[n - 1];
                        if 最后字节 == !握手字节序列[i] {
                            错位次数 = 0;
                            i += 1;
                        } else {
                            错位次数 += 1;
                            最近错位字节.push(最后字节);
                            if 最近错位字节.len() > 残留DA流检测窗口 {
                                最近错位字节.remove(0);
                            }
                            if 错位次数 <= 3 || handshake_should_restart_after_mismatch(错位次数)
                            {
                                info!(
                                    "[USB] handshake mismatch at byte {}: got 0x{:02X}, expected 0x{:02X}",
                                    i, 最后字节, !握手字节序列[i]
                                );
                            }
                            if looks_like_da_residual_stream(&最近错位字节) {
                                return Err(
                                    "BROM 握手读到疑似 DA/残留响应流 (A1/0B)。请长按电源 10 秒或重新插拔，确认设备重新进入干净 BROM 后再试。"
                                        .to_string(),
                                );
                            }
                            if handshake_should_restart_after_mismatch(错位次数) {
                                info!(
                                    "[USB] handshake mismatch 连续 {} 次，重新排空并开始下一轮",
                                    错位次数
                                );
                                成功 = false;
                                break;
                            }
                            i = 0; // Python 重置计数器
                        }
                    }
                    _ => {
                        info!("[USB] handshake read error at byte {}", i);
                        成功 = false;
                        break;
                    }
                }
            }
            if 成功 {
                info!("Handshake OK");
                return Ok(true);
            }
        }
        Err(format!(
            "Handshake failed after {} attempts",
            握手最大尝试次数
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_restarts_after_too_many_mismatches() {
        assert!(!handshake_should_restart_after_mismatch(
            单次握手最大错位次数 - 1
        ));
        assert!(handshake_should_restart_after_mismatch(
            单次握手最大错位次数
        ));
    }

    #[test]
    fn handshake_detects_da_residual_stream_pattern() {
        assert!(looks_like_da_residual_stream(&[0xA1, 0x0B, 0xA1, 0x0B]));
        assert!(!looks_like_da_residual_stream(&[0xA1, 0x0B, 0x5F, 0xF5]));
        assert!(!looks_like_da_residual_stream(&[0xA1, 0x0B, 0xA1]));
    }
}
