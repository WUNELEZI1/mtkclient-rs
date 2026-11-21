//! DA protocol engine: the full state machine converges on [`StreamPort`].
//!
//! Differences from `DAXFlash`:
//! - `DAXFlash` is bound to `Preloader`/`UsbDevice` and mixes in many USB special cases (reconnect, endpoints,
//!   short packets, `is_libusb` branches), so it cannot run in CI;
//! - this module only accepts `&mut dyn StreamPort`, so the same code can run on
//!   `transport::vdev` (virtual device), `transport::mock` (record/replay),
//!   or a future real serial/USB adapter.
//!
//! Scope (first phase): connect (handshake / chip identification / security flags) → DA upload →
//! storage open (CUSTOM_SET_STORAGE) → flash read. Write/erase/RPMB are not migrated yet.
//!
//! The wire protocol is identical to `DAXFlash` (command codes in [`crate::transport::wire`], with
//! conformance tests), so "passing in simulation" does not mean "a different protocol was invented".

// Submodules split by responsibility; public types and pure functions are re-exported from the module root to keep the external API unchanged.
mod flash;
mod protocol;
mod sim;
mod types;
mod util;

pub use types::{BromInfo, EngineError, Phase, StorageInfo};
pub use util::hexdump;

use log::{debug, trace};

use crate::transport::wire;
use crate::transport::{RetryPolicy, RetryingPort, StreamPort};

/// DA1 load address in SRAM (MTK convention).
pub const DA1_SRAM_ADDR: u32 = 0x0020_1000;

/// Protocol engine.
pub struct DaEngine {
    phase: Phase,
    info: Option<BromInfo>,
    storage: Option<StorageInfo>,
    /// Bounded retry policy for transient timeouts (Phase 1.1b). Defaults to 3 attempts; writes are not retried.
    retry: RetryPolicy,
}

impl Default for DaEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DaEngine {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            info: None,
            storage: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Set the retry policy (`--max-retries`).
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Wrap the retry port at the entry point so every internal `Self::rd` call gets bounded retries automatically.
    ///
    /// Wrap only at **public transaction entry points**; private primitives are not wrapped again, to avoid inflating the retry budget.
    #[inline]
    fn retrying<'a>(&self, port: &'a mut dyn StreamPort) -> RetryingPort<'a> {
        RetryingPort::new(port, self.retry)
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Chip information obtained during connect (for external tools / diagnostics).
    #[allow(dead_code)]
    pub fn info(&self) -> Option<BromInfo> {
        self.info
    }

    /// Storage information already queried.
    #[allow(dead_code)]
    pub fn storage(&self) -> Option<StorageInfo> {
        self.storage
    }

    // ── Low-level read/write (all with trace hex dumps) ─────────────────────

    fn wr(port: &mut dyn StreamPort, buf: &[u8]) -> Result<(), EngineError> {
        if log::log_enabled!(log::Level::Trace) {
            trace!("[engine] -> {}\n{}", buf.len(), hexdump(buf));
        }
        port.write_all(buf)?;
        Ok(())
    }

    fn rd(port: &mut dyn StreamPort, buf: &mut [u8]) -> Result<(), EngineError> {
        port.read_exact(buf)?;
        if log::log_enabled!(log::Level::Trace) {
            trace!("[engine] <- {}\n{}", buf.len(), hexdump(buf));
        }
        Ok(())
    }

    fn rd_vec(port: &mut dyn StreamPort, len: usize) -> Result<Vec<u8>, EngineError> {
        let mut v = vec![0u8; len];
        Self::rd(port, &mut v)?;
        Ok(v)
    }

    /// 1-byte command + echo check.
    fn echo1(port: &mut dyn StreamPort, cmd: u8) -> Result<(), EngineError> {
        Self::wr(port, &[cmd])?;
        let mut echo = [0u8; 1];
        Self::rd(port, &mut echo)?;
        if echo[0] != cmd {
            return Err(EngineError::Protocol(format!(
                "echo 0x{:02X} 期望回显 0x{:02X}，实际 0x{:02X}",
                cmd, cmd, echo[0]
            )));
        }
        Ok(())
    }

    /// 4-byte BE parameter + echo check.
    fn echo4(port: &mut dyn StreamPort, val: u32) -> Result<(), EngineError> {
        let be = val.to_be_bytes();
        Self::wr(port, &be)?;
        let echo = Self::rd_vec(port, 4)?;
        if echo != be {
            return Err(EngineError::Protocol(format!(
                "4 字节回显不匹配: 发 {:02X?} 收 {:02X?}",
                be, echo
            )));
        }
        Ok(())
    }

    fn rword_be(port: &mut dyn StreamPort) -> Result<u16, EngineError> {
        let b = Self::rd_vec(port, 2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    // ── XFlash basic primitives ────────────────────────────────────

    fn send_cmd(port: &mut dyn StreamPort, cmd: u32) -> Result<(), EngineError> {
        let mut pkt = [0u8; 12];
        pkt[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        pkt[4..8].copy_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        pkt[8..12].copy_from_slice(&4u32.to_le_bytes());
        Self::wr(port, &pkt)?;
        Self::wr(port, &cmd.to_le_bytes())
    }

    /// Send a group of parameters: one header per parameter, payload written in 0x200-byte chunks,
    /// then read status once after all writes (aligned with mtkclient `send_param`).
    fn send_params(port: &mut dyn StreamPort, params: &[&[u8]]) -> Result<(), EngineError> {
        for payload in params {
            let mut pkt = [0u8; 12];
            pkt[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
            pkt[4..8].copy_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
            pkt[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            Self::wr(port, &pkt)?;
            for chunk in payload.chunks(wire::XFLASH_PARAM_CHUNK) {
                Self::wr(port, chunk)?;
            }
        }
        Self::expect_status(port, "send_param")
    }

    fn send_payload(port: &mut dyn StreamPort, payload: &[u8]) -> Result<(), EngineError> {
        let mut pkt = [0u8; 12];
        pkt[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        pkt[4..8].copy_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        pkt[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        Self::wr(port, &pkt)?;
        Self::wr(port, payload)
    }

    /// Read one flow packet, skipping asynchronous device message packets.
    fn read_packet(port: &mut dyn StreamPort) -> Result<Vec<u8>, EngineError> {
        for _ in 0..64 {
            let hdr = Self::rd_vec(port, wire::XFLASH_HEADER_LEN)?;
            let magic = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let dtype = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[8..12].try_into().unwrap()) as usize;
            if magic != wire::XFLASH_MAGIC {
                return Err(EngineError::Protocol(format!(
                    "包头 magic 错误 0x{:08X}",
                    magic
                )));
            }
            if len > 64 * 1024 * 1024 {
                return Err(EngineError::Protocol(format!("包长异常 {}", len)));
            }
            let payload = Self::rd_vec(port, len)?;
            if dtype == wire::XFLASH_TYPE_MESSAGE {
                debug!(
                    "[engine] 设备消息: {}",
                    String::from_utf8_lossy(&payload).trim_end_matches(['\0', '\r', '\n'])
                );
                continue;
            }
            return Ok(payload);
        }
        Err(EngineError::Protocol("设备消息刷屏".into()))
    }

    fn status(port: &mut dyn StreamPort) -> Result<u32, EngineError> {
        let p = Self::read_packet(port)?;
        Ok(match p.len() {
            4 => u32::from_le_bytes(p[0..4].try_into().unwrap()),
            2 => u16::from_le_bytes(p[0..2].try_into().unwrap()) as u32,
            _ => 0,
        })
    }

    fn expect_status(port: &mut dyn StreamPort, what: &str) -> Result<(), EngineError> {
        let st = Self::status(port)?;
        if st != 0 {
            return Err(EngineError::Protocol(format!("{} 状态 0x{:08X}", what, st)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::vdev::VirtualDevice;

    fn da_bytes() -> Vec<u8> {
        // 256-byte fake DA (contains the MTK_DA_v5 marker for protocol version detection)
        let mut v = vec![0xAAu8; 256];
        v[0..8].copy_from_slice(b"MTK_DA_v");
        v
    }

    #[test]
    fn connect_reports_chip_and_safety() {
        let mut dev = VirtualDevice::from_preset("v5-sla").unwrap();
        let mut e = DaEngine::new();
        let info = e.connect(&mut dev).unwrap();
        assert_eq!(info.hw_code, 0x0707);
        assert_eq!(info.target_config & 0x2, 0x2, "SLA 位（0x2）应为 1");
        assert_eq!(e.phase(), Phase::Brom);
    }

    #[test]
    fn full_flow_reaches_gpt_signature() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        e.open_storage(&mut dev).unwrap();
        let gpt = e.read_flash(&mut dev, 0, 32768).unwrap();
        assert_eq!(&gpt[512..520], b"EFI PART");
    }

    #[test]
    fn sla_device_rejects_da_upload() {
        let mut dev = VirtualDevice::from_preset("v5-sla").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        assert_eq!(
            e.load_da(&mut dev, &da_bytes()),
            Err(EngineError::SlaRequired)
        );
    }

    #[test]
    fn exploit_unlocks_sla_device() {
        let mut dev = VirtualDevice::from_preset("v5-sla").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.sim_exploit(&mut dev, "kamakiri2").unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        e.open_storage(&mut dev).unwrap();
        assert_eq!(e.phase(), Phase::Ready);
    }

    #[test]
    fn storage_info_matches_profile() {
        let mut dev = VirtualDevice::from_preset("ufs-ab").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        let s = e.query_storage(&mut dev).unwrap();
        assert_eq!(s.emmc_type, 1);
        assert_eq!(s.type_name(), "UFS");
        assert_eq!(s.block, 4096);
    }

    #[test]
    fn read_before_da_is_rejected_locally() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        assert!(matches!(
            e.read_flash(&mut dev, 0, 512),
            Err(EngineError::OutOfOrder { .. })
        ));
    }

    #[test]
    fn emmc_user_size_is_at_offset_0x40() {
        // Layout matches mtkclient get_emmc_info: user_size is at 0x40 (0x20 is gp1).
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        let s = e.query_storage(&mut dev).unwrap();
        assert_eq!(s.user, dev.profile.storage.user);
        assert!(s.user > 0);
        assert_eq!(s.boot1, dev.profile.storage.boot1);
    }

    #[test]
    fn write_then_format_roundtrip() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        e.open_storage(&mut dev).unwrap();

        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        e.begin_write(&mut dev, 0x100000, payload.len() as u64, 1, 8)
            .unwrap();
        for chunk in payload.chunks(1024) {
            e.write_chunk(&mut dev, chunk).unwrap();
        }
        e.end_write(&mut dev).unwrap();

        let back = e.read_flash(&mut dev, 0x100000, 4096).unwrap();
        assert_eq!(back, payload);

        // After FORMAT, the readback should be 0xFF
        e.format(&mut dev, 8, 0x100000, 4096).unwrap();
        let blank = e.read_flash(&mut dev, 0x100000, 4096).unwrap();
        assert!(blank.iter().all(|b| *b == 0xFF));
    }

    #[test]
    fn write_chunk_must_be_512_aligned() {
        let mut dev = VirtualDevice::from_preset("v5-clean").unwrap();
        let mut e = DaEngine::new();
        e.connect(&mut dev).unwrap();
        e.load_da(&mut dev, &da_bytes()).unwrap();
        e.begin_write(&mut dev, 0x100000, 512, 1, 8).unwrap();
        let err = e.write_chunk(&mut dev, &[0u8; 100]).unwrap_err();
        assert!(err.to_string().contains("512"));
    }

    #[test]
    fn hexdump_layout() {
        let s = hexdump(b"AB");
        assert!(s.contains("00000000  41 42 "));
        assert!(s.contains("|AB|"));
    }
}
