//! fixture 回放框架（六段粒度）。
//!
//! 粒度对齐规划：`UsbEndpoint → BromHandshake → SocIdentify → HwSetup → DaPush → Readback`。
//! 存储格式为 JSON Lines，一行一段：
//!
//! ```json
//! {"segment":"brom_handshake","len":8,"bytes":"a1 25 ..."}
//! ```
//!
//! 用途：把真机抓到的“设备侧应答”保存下来，之后用 [`MockPort`] 脱机重放，
//! 让协议状态机在 CI 里跑，不依赖设备。

use std::io::{BufRead, BufReader, Cursor};

use crate::transport::mock::MockPort;

/// 回放粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Segment {
    /// USB 端点级（枚举 / 描述符）
    UsbEndpoint,
    /// BROM 握手（0xA0 / 0x0A 等）
    BromHandshake,
    /// SoC 识别（hw_code / hw_sub_code / sw_ver）
    SocIdentify,
    /// 硬件设置（DA 扩展 / 存储类型 / 安全配置）
    HwSetup,
    /// DA 下发
    DaPush,
    /// 回读（分区数据 / GPT）
    Readback,
}

impl Segment {
    pub const ALL: [Segment; 6] = [
        Segment::UsbEndpoint,
        Segment::BromHandshake,
        Segment::SocIdentify,
        Segment::HwSetup,
        Segment::DaPush,
        Segment::Readback,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Segment::UsbEndpoint => "usb_endpoint",
            Segment::BromHandshake => "brom_handshake",
            Segment::SocIdentify => "soc_identify",
            Segment::HwSetup => "hw_setup",
            Segment::DaPush => "da_push",
            Segment::Readback => "readback",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Segment::ALL.iter().copied().find(|x| x.key() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureError {
    BadLine(usize),
    UnknownSegment(usize),
    BadHex(usize),
    LenMismatch(usize),
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FixtureError::BadLine(n) => write!(f, "第 {} 行格式错误", n),
            FixtureError::UnknownSegment(n) => write!(f, "第 {} 行未知段名", n),
            FixtureError::BadHex(n) => write!(f, "第 {} 行 hex 非法", n),
            FixtureError::LenMismatch(n) => write!(f, "第 {} 行长度与 len 不一致", n),
        }
    }
}

impl std::error::Error for FixtureError {}

/// 一条 fixture。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fixture {
    pub segments: Vec<(Segment, Vec<u8>)>,
}

impl Fixture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, seg: Segment, bytes: impl Into<Vec<u8>>) {
        self.segments.push((seg, bytes.into()));
    }

    pub fn get(&self, seg: Segment) -> Option<&[u8]> {
        self.segments
            .iter()
            .find(|(s, _)| *s == seg)
            .map(|(_, b)| b.as_slice())
    }

    /// 序列化为 JSONL。
    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for (seg, bytes) in &self.segments {
            out.push_str(&format!(
                "{{\"segment\":\"{}\",\"len\":{},\"bytes\":\"{}\"}}\n",
                seg.key(),
                bytes.len(),
                to_hex(bytes)
            ));
        }
        out
    }

    /// 从 JSONL 解析。容忍空行与前后空白，未知段报错（不静默丢弃）。
    pub fn from_jsonl(text: &str) -> Result<Self, FixtureError> {
        let mut fx = Fixture::new();
        let reader = BufReader::new(Cursor::new(text));
        for (i, line) in reader.lines().enumerate() {
            let line = line.map_err(|_| FixtureError::BadLine(i + 1))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let seg_key = extract_str(line, "segment").ok_or(FixtureError::BadLine(i + 1))?;
            let seg = Segment::from_key(seg_key).ok_or(FixtureError::UnknownSegment(i + 1))?;
            let len: usize = extract_num(line, "len")
                .and_then(|s: &str| s.parse::<usize>().ok())
                .ok_or(FixtureError::BadLine(i + 1))?;
            let hex = extract_str(line, "bytes").ok_or(FixtureError::BadLine(i + 1))?;
            let bytes = parse_hex(hex).ok_or(FixtureError::BadHex(i + 1))?;
            if bytes.len() != len {
                return Err(FixtureError::LenMismatch(i + 1));
            }
            fx.push(seg, bytes);
        }
        Ok(fx)
    }

    /// 把各段按顺序灌入 mock 的 rx（模拟设备侧连续应答）。
    pub fn replay(&self, port: &mut MockPort) {
        for (_, bytes) in &self.segments {
            port.push_rx(bytes);
        }
    }

    /// 校验段顺序是否为标准六段（可缺段，但不得乱序）。
    pub fn is_ordered(&self) -> bool {
        let mut last = None;
        for (seg, _) in &self.segments {
            if let Some(prev) = last {
                if *seg < prev {
                    return false;
                }
            }
            last = Some(*seg);
        }
        true
    }
}

fn to_hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{:02x}", x))
        .collect::<Vec<_>>()
        .join("")
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() % 2 != 0 {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}

fn extract_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{}\":\"", key);
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn extract_num<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{}\":", key);
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

#[allow(dead_code)] // 供后续 ops 脱机回归使用
/// 最小 printgpt fixture：BROM 握手 → SoC 识别 → DA 下发 → 回读 GPT 头。
///
/// 仅用于验证“fixture → bytes → GPT 签名”的闭环，不模拟完整协议状态机。
pub fn printgpt_fixture() -> Fixture {
    let mut fx = Fixture::new();
    fx.push(Segment::UsbEndpoint, vec![0x12, 0x01, 0x00, 0x02]);
    fx.push(Segment::BromHandshake, vec![0xA0, 0x0A, 0x50, 0x05]);
    fx.push(Segment::SocIdentify, vec![0x07, 0x07, 0x68, 0x67]);
    fx.push(Segment::HwSetup, vec![0x00, 0x00, 0x01, 0x00]);
    fx.push(Segment::DaPush, vec![0xDA, 0xDA, 0x00, 0x00]);
    // GPT 头：签名 "EFI PART" + 版本，其余补零到 512
    let mut gpt = vec![0u8; 512];
    gpt[..8].copy_from_slice(b"EFI PART");
    gpt[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    fx.push(Segment::Readback, gpt);
    fx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::StreamPort;

    #[test]
    fn jsonl_roundtrip() {
        let fx = printgpt_fixture();
        let text = fx.to_jsonl();
        assert_eq!(text.lines().count(), 6);
        let back = Fixture::from_jsonl(&text).unwrap();
        assert_eq!(back, fx);
        assert!(back.is_ordered());
    }

    #[test]
    fn get_returns_named_segment() {
        let mut fx = Fixture::new();
        fx.push(Segment::SocIdentify, vec![0xAA, 0xBB]);
        assert_eq!(fx.get(Segment::SocIdentify), Some(&[0xAA, 0xBB][..]));
        assert_eq!(fx.get(Segment::Readback), None);
    }

    #[test]
    fn unknown_segment_rejected() {
        let line = "{\"segment\":\"nope\",\"len\":1,\"bytes\":\"00\"}\n";
        assert_eq!(
            Fixture::from_jsonl(line),
            Err(FixtureError::UnknownSegment(1))
        );
    }

    #[test]
    fn length_mismatch_rejected() {
        let line = "{\"segment\":\"readback\",\"len\":4,\"bytes\":\"0000\"}\n";
        assert_eq!(Fixture::from_jsonl(line), Err(FixtureError::LenMismatch(1)));
    }

    #[test]
    fn replay_feeds_mock_and_gpt_signature_visible() {
        let fx = printgpt_fixture();
        let mut port = MockPort::new(Vec::new());
        fx.replay(&mut port);

        // 顺序读出全部段，最后一段应含 GPT 签名
        let mut all = vec![0u8; 4 + 4 + 4 + 4 + 4 + 512];
        port.read_exact(&mut all).unwrap();
        let gpt = &all[all.len() - 512..];
        assert_eq!(&gpt[..8], b"EFI PART");
        assert_eq!(
            u32::from_le_bytes(gpt[8..12].try_into().unwrap()),
            0x0001_0000
        );
    }

    #[test]
    fn unordered_detected() {
        let mut fx = Fixture::new();
        fx.push(Segment::Readback, vec![0]);
        fx.push(Segment::BromHandshake, vec![0]);
        assert!(!fx.is_ordered());
    }
}
