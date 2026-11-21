//! Fixture replay framework (six-segment granularity).
//!
//! Granularity follows the planning stages: `UsbEndpoint → BromHandshake → SocIdentify → HwSetup → DaPush → Readback`.
//! Storage format is JSON Lines, one segment per line:
//!
//! ```json
//! {"segment":"brom_handshake","len":8,"bytes":"a1 25 ..."}
//! ```
//!
//! Purpose: save device-side responses captured from real hardware, then replay them offline with [`MockPort`],
//! so the protocol state machine runs in CI without a device.

use std::io::{BufRead, BufReader, Cursor};

use crate::transport::mock::MockPort;

/// Replay granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Segment {
    /// USB endpoint level (enumeration / descriptors)
    UsbEndpoint,
    /// BROM handshake (0xA0 / 0x0A, etc.)
    BromHandshake,
    /// SoC identification (hw_code / hw_sub_code / sw_ver)
    SocIdentify,
    /// Hardware setup (DA extensions / storage type / security config)
    HwSetup,
    /// DA push
    DaPush,
    /// Readback (partition data / GPT)
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

/// A single fixture.
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

    /// Serialize to JSONL.
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

    /// Parse from JSONL. Tolerates blank lines and surrounding whitespace; unknown segments are errors (not silently dropped).
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

    /// Feed the segments in order into the mock's rx (simulating consecutive device responses).
    pub fn replay(&self, port: &mut MockPort) {
        for (_, bytes) in &self.segments {
            port.push_rx(bytes);
        }
    }

    /// Check whether segments follow the standard six-segment order (segments may be missing but must not be out of order).
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

#[allow(dead_code)] // for later offline ops regression tests
/// Minimal printgpt fixture: BROM handshake → SoC identification → DA push → read back the GPT header.
///
/// Only validates the "fixture → bytes → GPT signature" loop; it does not model the full protocol state machine.
pub fn printgpt_fixture() -> Fixture {
    let mut fx = Fixture::new();
    fx.push(Segment::UsbEndpoint, vec![0x12, 0x01, 0x00, 0x02]);
    fx.push(Segment::BromHandshake, vec![0xA0, 0x0A, 0x50, 0x05]);
    fx.push(Segment::SocIdentify, vec![0x07, 0x07, 0x68, 0x67]);
    fx.push(Segment::HwSetup, vec![0x00, 0x00, 0x01, 0x00]);
    fx.push(Segment::DaPush, vec![0xDA, 0xDA, 0x00, 0x00]);
    // GPT header: "EFI PART" signature + version, zero-padded to 512 bytes
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

        // Read all segments in order; the last one should contain the GPT signature
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
