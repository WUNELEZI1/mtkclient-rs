//! 协议原语 — 块 1: 状态 / 数据读取

use super::{CMD_MAGIC, DA_CTRL_TIMEOUT_MS, DATA_TYPE_MESSAGE};
use log::{debug, trace};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

/// XFlash 包头长度：magic(4) + data_type(4) + length(4)
const XFLASH_HEADER_LEN: usize = 12;
/// 单次响应读取中最多跳过的 Message 包数量。
/// 防御性上限：正常 DA 不会连续吐这么多日志包，超出即视为协议失步，
/// 避免设备异常时陷入无界循环。
const MAX_MESSAGE_PACKETS_PER_READ: u32 = 64;

/// 单包长度的合理上限（64 MiB）。仅作防御性 sanity check：
/// DA 正常单包远小于此值（协商后 2~4 MiB），若包头被错位解析出
/// 0xFFFFFFFF 之类的长度，直接分配会 OOM/panic，故先拒收。
const MAX_XFLASH_PACKET_LEN: u32 = 64 * 1024 * 1024;

/// 包头声明长度是否在合理范围内（纯函数，便于单测）
pub(crate) fn packet_length_is_valid(length: u32) -> bool {
    length <= MAX_XFLASH_PACKET_LEN
}

/// 收到一个包后应如何处理
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacketAction {
    /// 设备异步消息包：消费负载并记录，然后继续读下一个包
    SkipMessage,
    /// 流程包（或未知类型）：作为本次响应返回（对未知类型保持宽容，
    /// 与旧实现一致，避免误伤新固件的扩展类型）
    Deliver,
}

/// 根据包头 `data_type` 决定处理方式（纯函数，便于单测）
pub(crate) fn classify_packet(data_type: u32) -> PacketAction {
    if data_type == DATA_TYPE_MESSAGE {
        PacketAction::SkipMessage
    } else {
        PacketAction::Deliver
    }
}

/// 将设备异步消息负载渲染为可读文本（纯函数，便于单测）
pub(crate) fn format_device_message(payload: &[u8]) -> String {
    String::from_utf8_lossy(payload)
        .trim_end_matches(['\0', '\r', '\n'])
        .to_string()
}

/// 处理一个已读入的包：
/// - Message 包：记录日志、`skipped` 计数 +1，返回 `Ok(None)`（表示继续读下一个包）
/// - 流程包：返回 `Ok(Some(payload))`（作为本次响应）
/// - 连续 Message 包超过上限：返回 `Err`（协议失步）
///
/// 纯函数（不碰 I/O），便于单测覆盖跳包与上限逻辑。
fn handle_packet(
    data_type: u32,
    payload: Vec<u8>,
    skipped: &mut u32,
) -> Result<Option<Vec<u8>>, String> {
    match classify_packet(data_type) {
        PacketAction::SkipMessage => {
            *skipped += 1;
            debug!(
                "[xread] 设备异步消息(len={}): {}",
                payload.len(),
                format_device_message(&payload)
            );
            if *skipped >= MAX_MESSAGE_PACKETS_PER_READ {
                return Err(
                    crate::error::ProtocolError::MessageFlood { count: *skipped }.to_string(),
                );
            }
            Ok(None)
        }
        PacketAction::Deliver => Ok(Some(payload)),
    }
}

impl<'a> DAXFlash<'a> {
    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    pub(crate) fn xread(&mut self) -> Result<u32, String> {
        // 设备处理 setup_hw_init 后可能需要数百毫秒才返回响应。
        // 用 max(当前超时, DA_CTRL_TIMEOUT_MS) 等待首字节，避免慢速串口 status 超时；
        // 若调用方已设更短超时（with_short_timeout 快速失败），保留之。
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = self.xread_inner();
        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    fn xread_inner(&mut self) -> Result<u32, String> {
        let payload = self.read_flow_packet()?;
        // 如果数据是 4 字节，返回 u32 值
        if payload.len() == 4 {
            let val = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            trace!("[xread] val=0x{:08X}", val);
            return Ok(val);
        }
        Ok(0)
    }

    /// 读取单个 XFlash 包，返回 (data_type, payload)
    fn read_packet(&mut self) -> Result<(u32, Vec<u8>), String> {
        let mut header = [0u8; XFLASH_HEADER_LEN];
        self.preloader.device.read_exact(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            trace!("[xread] bad magic: 0x{:08X}", magic);
            return Err(crate::error::ProtocolError::BadMagic {
                got: magic,
                expected: CMD_MAGIC,
            }
            .to_string());
        }

        if !packet_length_is_valid(length) {
            trace!("[xread] invalid length: {}", length);
            return Err(crate::error::ProtocolError::InvalidPacketLength(length).to_string());
        }

        trace!("[xread] dt={:08X} len={}", data_type, length);

        let mut payload = vec![0u8; length as usize];
        if length > 0 {
            self.preloader.device.read_exact(&mut payload)?;
        }
        Ok((data_type, payload))
    }

    /// 读取下一个流程包，途中自动跳过并记录设备异步 Message 包（data_type=2）。
    ///
    /// DA 可在任意时刻插入日志/状态消息包；旧实现把这些消息的负载当成
    /// 命令响应消费，导致后续包头错位（协议失步）。此处在包头解析后判断
    /// `data_type`，遇到 Message 先消费其负载并记录，直到拿到流程包才返回。
    /// 参考 penumbra `read_next_flow_header()` + `drain_message()`。
    fn read_flow_packet(&mut self) -> Result<Vec<u8>, String> {
        let mut skipped = 0u32;
        loop {
            let (data_type, payload) = self.read_packet()?;
            if let Some(payload) = handle_packet(data_type, payload, &mut skipped)? {
                return Ok(payload);
            }
        }
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        // DA 控制读：慢速串口下 DA 回 status 可能超默认 1s，用 max(当前, DA_CTRL_TIMEOUT_MS)
        // 等待首字节，避免 status 读取 Operation timed out。调用方已设更短超时时保留之。
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            // 跳过 DA 异步消息包后再取流程包（避免消息负载被当成 status 解析）
            let payload = self.read_flow_packet()?;
            match payload.len() {
                4 => {
                    let val = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                    // Python 特殊情况：如果 status == 0xFEEEEEEF，返回 0
                    if val == 0xFEEEEEEF {
                        trace!("[status] 0xFEEEEEEF → 0");
                        return Ok(0);
                    }
                    trace!("[status] val=0x{:08X}", val);
                    Ok(val)
                }
                2 => {
                    let val = u16::from_le_bytes(payload[0..2].try_into().unwrap()) as u32;
                    trace!("[status] val=0x{:04X}", val);
                    Ok(val)
                }
                other => {
                    trace!("[status] len={} (no u32/u16 payload)", other);
                    Ok(0)
                }
            }
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        // DA 控制读：同 status()，用 max(当前, DA_CTRL_TIMEOUT_MS) 避免慢速串口超时
        let orig_timeout = self.preloader.device.get_timeout();
        let eff = orig_timeout.max(Duration::from_millis(DA_CTRL_TIMEOUT_MS));
        self.preloader.device.set_timeout(eff);

        let result = (|| {
            // 跳过 DA 异步消息包后再取流程包
            let payload = self.read_flow_packet()?;
            trace!("[xread_data] len={}", payload.len());
            Ok(payload)
        })();

        self.preloader.device.set_timeout(orig_timeout);
        result
    }
}

// =============================================================================
// 单元测试 — Message 包处理（P0-1）
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::protocol::{DATA_TYPE_FLOW, DATA_TYPE_MESSAGE};

    #[test]
    fn classify_packet_distinguishes_message_and_flow() {
        assert_eq!(
            classify_packet(DATA_TYPE_MESSAGE),
            PacketAction::SkipMessage
        );
        assert_eq!(classify_packet(DATA_TYPE_FLOW), PacketAction::Deliver);
        // 未知类型保持宽容，按流程包处理（与旧行为一致）
        assert_eq!(classify_packet(0x55), PacketAction::Deliver);
        assert_eq!(classify_packet(0), PacketAction::Deliver);
    }

    #[test]
    fn format_device_message_trims_trailing_control_bytes() {
        assert_eq!(format_device_message(b"hello\n"), "hello");
        assert_eq!(format_device_message(b"hello\0"), "hello");
        assert_eq!(format_device_message(b"hello\r\n"), "hello");
        assert_eq!(format_device_message(b""), "");
        // 非 UTF-8 也能安全降级（lossy）
        assert_eq!(format_device_message(&[0xFF, b'o', b'k']), "\u{FFFD}ok");
    }

    #[test]
    fn handle_packet_skips_message_then_delivers_flow() {
        let mut skipped = 0u32;
        // 两个消息包 → 返回 None（继续读）
        assert!(
            handle_packet(DATA_TYPE_MESSAGE, b"log1".to_vec(), &mut skipped)
                .unwrap()
                .is_none()
        );
        assert!(
            handle_packet(DATA_TYPE_MESSAGE, b"log2".to_vec(), &mut skipped)
                .unwrap()
                .is_none()
        );
        assert_eq!(skipped, 2);
        // 流程包 → 返回负载，且 skipped 不再增长
        let delivered = handle_packet(DATA_TYPE_FLOW, vec![1, 2, 3, 4], &mut skipped)
            .unwrap()
            .unwrap();
        assert_eq!(delivered, vec![1, 2, 3, 4]);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn handle_packet_errors_on_message_flood() {
        let mut skipped = 0u32;
        // 连续 MAX-1 个消息包仍应正常跳过
        for _ in 0..(MAX_MESSAGE_PACKETS_PER_READ - 1) {
            assert!(
                handle_packet(DATA_TYPE_MESSAGE, vec![], &mut skipped)
                    .unwrap()
                    .is_none()
            );
        }
        // 第 MAX 个触发协议失步错误
        let err = handle_packet(DATA_TYPE_MESSAGE, vec![], &mut skipped).unwrap_err();
        assert!(err.contains("Message"), "unexpected error: {}", err);
    }

    #[test]
    fn packet_length_guard_rejects_absurd_lengths() {
        assert!(packet_length_is_valid(0));
        assert!(packet_length_is_valid(0x200000)); // 2MiB 正常值
        assert!(packet_length_is_valid(MAX_XFLASH_PACKET_LEN));
        // 错位解析出的荒谬长度被拒收，避免 vec![0u8; 4GiB] 直接 OOM
        assert!(!packet_length_is_valid(MAX_XFLASH_PACKET_LEN + 1));
        assert!(!packet_length_is_valid(0xFFFFFFFF));
    }

    #[test]
    fn read_packet_header_layout_matches_pack3() {
        // 用 pack3 生成的头应与读包逻辑的字段偏移一致（回归保护）
        let pkt = crate::da::xflash::protocol::pack3(CMD_MAGIC, DATA_TYPE_MESSAGE, 5);
        let magic = u32::from_le_bytes(pkt[0..4].try_into().unwrap());
        let data_type = u32::from_le_bytes(pkt[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(pkt[8..12].try_into().unwrap());
        assert_eq!(magic, CMD_MAGIC);
        assert_eq!(data_type, DATA_TYPE_MESSAGE);
        assert_eq!(length, 5);
        assert_eq!(XFLASH_HEADER_LEN, pkt.len());
    }
}
