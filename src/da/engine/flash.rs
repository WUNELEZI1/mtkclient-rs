//! XFlash read/write, erase, and storage query/open.

use log::{debug, trace, warn};

use crate::transport::wire;
use crate::transport::{PortError, StreamPort};

use super::{DaEngine, EngineError, Phase, StorageInfo};

impl DaEngine {
    /// Parameterless devctrl command.
    #[allow(dead_code)]
    pub fn devctrl(port: &mut dyn StreamPort, cmd: u32) -> Result<(), EngineError> {
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, cmd)?;
        Self::expect_status(port, "devctrl cmd")?;
        Ok(())
    }

    /// devctrl command with parameters.
    pub fn devctrl_param(
        port: &mut dyn StreamPort,
        cmd: u32,
        param: &[u8],
    ) -> Result<(), EngineError> {
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, cmd)?;
        Self::expect_status(port, "devctrl cmd")?;
        Self::send_payload(port, param)?;
        Self::expect_status(port, "devctrl param")?;
        Ok(())
    }

    /// Read flash (single READ_DATA, length limited by packet size; caller splits chunks).
    pub fn read_flash(
        &mut self,
        port: &mut dyn StreamPort,
        addr: u64,
        size: u64,
    ) -> Result<Vec<u8>, EngineError> {
        if !matches!(self.phase, Phase::Da1 | Phase::Ready) {
            return Err(EngineError::OutOfOrder {
                phase: self.phase,
                want: "DA 已加载",
            });
        }
        let policy = self.retry;
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        let mut attempt = 0u32;
        loop {
            match Self::read_flash_once(port, addr, size) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    // Reads are idempotent: truncation / misalignment / transient timeouts are handled by
                    // flushing the input buffer and resending READ_DATA from scratch. That is the right
                    // layer against packet loss (byte-level continuation cannot recover lost bytes).
                    let retryable = matches!(
                        e,
                        EngineError::Port(PortError::Timeout) | EngineError::Protocol(_)
                    );
                    if !retryable || attempt >= policy.max_retries {
                        return Err(e);
                    }
                    attempt += 1;
                    warn!(
                        "[engine] READ_DATA 失败（{}），清空输入缓冲后重试 {}/{}",
                        e, attempt, policy.max_retries
                    );
                    let _ = port.drain();
                    let d = policy.delay(attempt);
                    if !d.is_zero() {
                        std::thread::sleep(d);
                    }
                }
            }
        }
    }

    /// Single READ_DATA transaction (no retries).
    fn read_flash_once(
        port: &mut dyn StreamPort,
        addr: u64,
        size: u64,
    ) -> Result<Vec<u8>, EngineError> {
        Self::send_cmd(port, wire::XFLASH_CMD_READ_DATA)?;
        Self::expect_status(port, "READ_DATA cmd")?;

        let mut param = [0u8; wire::XFLASH_READ_PARAM_LEN];
        param[..4].copy_from_slice(&1u32.to_le_bytes());
        param[4..8].copy_from_slice(&8u32.to_le_bytes()); // parttype=8 physical partition
        param[8..16].copy_from_slice(&addr.to_le_bytes());
        param[16..24].copy_from_slice(&size.to_le_bytes());
        Self::send_payload(port, &param)?;
        Self::expect_status(port, "READ_DATA param")?;

        let data = Self::read_packet(port)?;
        Self::expect_status(port, "READ_DATA data")?;
        if data.len() as u64 != size {
            return Err(EngineError::Protocol(format!(
                "READ_DATA 返回长度不符: 期望 {} 实际 {}",
                size,
                data.len()
            )));
        }
        Ok(data)
    }

    // ── Stage 3: write / erase ──────────────────────────────

    /// Start a write transaction: WRITE_DATA + 56-byte parameters.
    ///
    /// Parameter layout (matches mtkclient `cmd_write_data`):
    /// storage(4) parttype(4) addr(8) size(8) + 32-byte NandExtension (all zero).
    /// Then feed each chunk with [`Self::write_chunk`] and finish with [`Self::end_write`].
    pub fn begin_write(
        &mut self,
        port: &mut dyn StreamPort,
        addr: u64,
        size: u64,
        storage: u32,
        parttype: u32,
    ) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        if !matches!(self.phase, Phase::Da1 | Phase::Ready) {
            return Err(EngineError::OutOfOrder {
                phase: self.phase,
                want: "DA 已加载",
            });
        }
        Self::send_cmd(port, wire::XFLASH_CMD_WRITE_DATA)?;
        Self::expect_status(port, "WRITE_DATA cmd")?;

        let mut param = [0u8; wire::XFLASH_WRITE_PARAM_LEN];
        param[0..4].copy_from_slice(&storage.to_le_bytes());
        param[4..8].copy_from_slice(&parttype.to_le_bytes());
        param[8..16].copy_from_slice(&addr.to_le_bytes());
        param[16..24].copy_from_slice(&size.to_le_bytes());
        // 24..56 is the NandExtension (all zero for eMMC)
        Self::send_params(port, &[&param])?;
        trace!(
            "[engine] WRITE_DATA 开始: addr=0x{:X} size={} parttype={}",
            addr, size, parttype
        );
        Ok(())
    }

    /// Write one chunk: parameter list [0u32, checksum, data].
    pub fn write_chunk(
        &mut self,
        port: &mut dyn StreamPort,
        data: &[u8],
    ) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        if data.is_empty() {
            return Ok(());
        }
        if !data.len().is_multiple_of(512) {
            return Err(EngineError::Protocol(format!(
                "写入分块必须 512 字节对齐，实际 {}",
                data.len()
            )));
        }
        let zero = 0u32.to_le_bytes();
        // Matches mtkclient: checksum = sum(data) & 0xFFFF
        let checksum = (crate::da::xflash::chunk_checksum(data) & 0xFFFF).to_le_bytes();
        Self::send_params(port, &[&zero, &checksum, data])?;
        Ok(())
    }

    /// End the write transaction: final status + CC_OPTIONAL_DOWNLOAD_ACT.
    pub fn end_write(&mut self, port: &mut dyn StreamPort) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        Self::expect_status(port, "WRITE_DATA final")?;
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, wire::XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT)?;
        Self::expect_status(port, "CC_OPTIONAL_DOWNLOAD_ACT")?;
        trace!("[engine] WRITE_DATA 事务完成");
        Ok(())
    }

    /// Erase a physical region (FORMAT).
    ///
    /// The device may return `STATUS_CONTINUE` with a millisecond wait; as in mtkclient,
    /// sleep + ack and re-read the status until `STATUS_COMPLETE`.
    pub fn format(
        &mut self,
        port: &mut dyn StreamPort,
        parttype: u32,
        addr: u64,
        size: u64,
    ) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        if !matches!(self.phase, Phase::Da1 | Phase::Ready) {
            return Err(EngineError::OutOfOrder {
                phase: self.phase,
                want: "DA 已加载",
            });
        }
        Self::send_cmd(port, wire::XFLASH_CMD_FORMAT)?;
        Self::expect_status(port, "FORMAT cmd")?;

        let mut param = [0u8; wire::XFLASH_FORMAT_PARAM_LEN];
        param[0..4].copy_from_slice(&1u32.to_le_bytes()); // storage=1 (eMMC/UFS)
        param[4..8].copy_from_slice(&parttype.to_le_bytes());
        param[8..16].copy_from_slice(&addr.to_le_bytes());
        param[16..24].copy_from_slice(&size.to_le_bytes());
        Self::send_params(port, &[&param])?;

        let mut st = Self::status(port)?;
        let mut waits = 0u32;
        while st == wire::XFLASH_STATUS_CONTINUE {
            // The device reports the wait in milliseconds with "the next 4 bytes"
            let wait_ms = Self::status(port)?.min(5_000) as u64;
            waits += 1;
            if waits > 600 {
                return Err(EngineError::Protocol("FORMAT 等待超时".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            Self::send_payload(port, &0u32.to_le_bytes())?; // ack
            st = Self::status(port)?;
        }
        if st != wire::XFLASH_STATUS_COMPLETE && st != 0 {
            return Err(EngineError::Protocol(format!(
                "FORMAT 失败: status=0x{:08X}",
                st
            )));
        }
        if st == wire::XFLASH_STATUS_COMPLETE {
            // An ack is mandatory after COMPLETE, otherwise the DA stays stuck waiting
            Self::send_payload(port, &0u32.to_le_bytes())?;
            let _ = Self::status(port);
        }
        trace!("[engine] FORMAT 完成: addr=0x{:X} size={}", addr, size);
        Ok(())
    }

    // ── Stage 3: open storage ───────────────────────────────

    /// Query storage info (eMMC / UFS capacity and block size).
    pub fn query_storage(&mut self, port: &mut dyn StreamPort) -> Result<StorageInfo, EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        if !matches!(self.phase, Phase::Da1 | Phase::Ready) {
            return Err(EngineError::OutOfOrder {
                phase: self.phase,
                want: "DA 已加载",
            });
        }
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, wire::XFLASH_CMD_EMMC_INFO)?;
        Self::expect_status(port, "EMMC_INFO")?;
        let payload = Self::read_packet(port)?;
        let _ = Self::status(port);
        if payload.len() < 40 {
            return Err(EngineError::Protocol(format!(
                "eMMC 信息长度不足: {}",
                payload.len()
            )));
        }
        let rd = |o: usize| {
            if payload.len() >= o + 8 {
                u64::from_le_bytes(payload[o..o + 8].try_into().unwrap())
            } else {
                0
            }
        };
        // Layout (matches mtkclient `get_emmc_info`):
        //   0 type(4) 4 block(4) 8 boot1(8) 16 boot2(8) 24 rpmb(8)
        //   32 gp1 40 gp2 48 gp3 56 gp4 64 user(8) 72 cid(16) 88 fwver(8)
        // An earlier version read user at 32 (actually gp1), which showed a zero capacity and hid
        // boundary problems such as partitions beyond the disk end.
        let info = StorageInfo {
            emmc_type: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            block: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            boot1: rd(8),
            boot2: rd(16),
            user: rd(64),
        };
        self.storage = Some(info);
        debug!(
            "[engine] 存储: {} block={} boot1=0x{:X} user=0x{:X}",
            info.type_name(),
            info.block,
            info.boot1,
            info.user
        );
        Ok(info)
    }

    /// Open storage: CUSTOM_SET_STORAGE (requires DA extensions to be loaded).
    pub fn open_storage(&mut self, port: &mut dyn StreamPort) -> Result<(), EngineError> {
        // The storage query is best-effort: old/custom DAs may reject 0x00040001,
        // but as long as READ_DATA works printgpt should succeed (same as cmd_printgpt on devices).
        let storage = match self.storage {
            Some(s) => Some(s),
            None => match self.query_storage(port) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!("[engine] 存储信息查询失败（继续，容量字段显示为 0）: {}", e);
                    None
                }
            },
        };
        if let Some(s) = storage
            && let Err(e) = Self::devctrl_param(
                port,
                wire::XFLASH_DEVCTRL_SET_STORAGE,
                &s.storage_type().to_le_bytes(),
            )
        {
            // On devices without DA extensions this is expected: storage stays readable, only extensions are limited
            warn!("[engine] CUSTOM_SET_STORAGE 失败（可忽略）: {}", e);
        }
        self.phase = Phase::Ready;
        Ok(())
    }
}
