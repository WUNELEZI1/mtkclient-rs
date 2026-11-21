//! Connection handshake, DA upload and DA extension handshake.

use log::{debug, trace, warn};

use crate::transport::StreamPort;
use crate::transport::wire;

use super::{BromInfo, DA1_SRAM_ADDR, DaEngine, EngineError, Phase};

impl DaEngine {
    // ── Stage 1: BROM connect ───────────────────────────────

    /// Handshake + chip identification + safety flag read.
    pub fn connect(&mut self, port: &mut dyn StreamPort) -> Result<BromInfo, EngineError> {
        // Wrap the entry in bounded retries: any transient "no byte yet" timeout during
        // the handshake is retried automatically.
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        // Handshake: send byte by byte, the device echoes the bitwise complement
        for b in wire::BROM_HANDSHAKE {
            Self::wr(port, &[b])?;
            let mut echo = [0u8; 1];
            Self::rd(port, &mut echo)?;
            if echo[0] != !b {
                return Err(EngineError::Protocol(format!(
                    "握手失败: 发 0x{:02X} 期望 0x{:02X} 收 0x{:02X}",
                    b, !b, echo[0]
                )));
            }
        }
        trace!("[engine] 握手完成");

        // BROM sync (FE): the device returns 1 byte; the value is not checked
        Self::wr(port, &[0xFE])?;
        let _ = Self::rd_vec(port, 1)?;

        // Chip identification
        Self::echo1(port, wire::BROM_GET_HW_CODE)?;
        let hw = Self::rd_vec(port, 4)?;
        let hw_code = u16::from_be_bytes([hw[0], hw[1]]);
        let hw_sub = u16::from_be_bytes([hw[2], hw[3]]);

        // Safety flags
        Self::echo1(port, wire::BROM_GET_TARGET_CONFIG)?;
        let cfg = Self::rd_vec(port, 6)?;
        let target_config = u32::from_be_bytes([cfg[0], cfg[1], cfg[2], cfg[3]]);
        let status = u16::from_be_bytes([cfg[4], cfg[5]]);
        if status > 0xFF {
            warn!("[engine] target config status=0x{:04X}", status);
        }

        let info = BromInfo {
            hw_code,
            hw_sub,
            target_config,
        };
        self.info = Some(info);
        self.phase = Phase::Brom;
        debug!(
            "[engine] 连接成功: hw=0x{:04X} sub=0x{:04X} safety=0x{:08X}",
            hw_code, hw_sub, target_config
        );
        Ok(info)
    }

    // ── Stage 2: DA upload ─────────────────────────────────

    /// Uploads DA1 and jumps to it, then waits for the DA's XFlash SYNC.
    pub fn load_da(&mut self, port: &mut dyn StreamPort, da: &[u8]) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        if self.phase == Phase::Idle {
            return Err(EngineError::OutOfOrder {
                phase: self.phase,
                want: "已连接（connect）",
            });
        }
        if da.is_empty() {
            return Err(EngineError::Protocol("DA 文件为空".into()));
        }
        self.send_da(port, DA1_SRAM_ADDR, da, 0)?;
        self.jump_da(port, DA1_SRAM_ADDR)?;
        self.wait_sync(port)?;
        self.phase = Phase::Da1;
        debug!("[engine] DA1 已运行（{} 字节）", da.len());
        Ok(())
    }

    /// BROM SEND_DA。
    fn send_da(
        &mut self,
        port: &mut dyn StreamPort,
        addr: u32,
        data: &[u8],
        sig_len: u32,
    ) -> Result<(), EngineError> {
        Self::echo4(port, wire::BROM_SEND_DA as u32)?;
        Self::echo4(port, addr)?;
        Self::echo4(port, data.len() as u32)?;
        Self::echo4(port, sig_len)?;
        let status = Self::rword_be(port)?;
        if status == wire::BROM_STATUS_SLA_REQUIRED {
            return Err(EngineError::SlaRequired);
        }
        if status > 0xFF {
            return Err(EngineError::Protocol(format!(
                "SEND_DA 状态错误 0x{:04X}",
                status
            )));
        }
        // Data body (chunked at 64 KiB to mimic real fragmentation; the device accumulates byte by byte)
        for chunk in data.chunks(64 * 1024) {
            Self::wr(port, chunk)?;
        }
        // ZLP
        port.flush()?;
        let checksum = Self::rword_be(port)?;
        let status2 = Self::rword_be(port)?;
        trace!(
            "[engine] SEND_DA 完成: {} 字节 checksum=0x{:04X} status=0x{:04X}",
            data.len(),
            checksum,
            status2
        );
        if status2 > 0xFF {
            return Err(EngineError::Protocol(format!(
                "SEND_DA 收尾状态错误 0x{:04X}",
                status2
            )));
        }
        Ok(())
    }

    /// BROM JUMP_DA。
    fn jump_da(&mut self, port: &mut dyn StreamPort, addr: u32) -> Result<(), EngineError> {
        Self::echo1(port, wire::BROM_JUMP_DA)?;
        Self::echo4(port, addr)?;
        Self::wr(port, &addr.to_be_bytes())?;
        // Wait for 0xC0 (tolerating leading 0x00 padding)
        for _ in 0..8 {
            let b = Self::rd_vec(port, 1)?;
            if b[0] == wire::BROM_JUMP_ACK {
                trace!("[engine] JUMP_DA 收到 0xC0");
                return Ok(());
            }
            trace!("[engine] JUMP_DA 跳过 0x{:02X}", b[0]);
        }
        Err(EngineError::Protocol("JUMP_DA 未收到 0xC0".into()))
    }

    /// Waits for and consumes the DA SYNC signal.
    fn wait_sync(&mut self, port: &mut dyn StreamPort) -> Result<(), EngineError> {
        let word = Self::rd_vec(port, 4)?;
        let val = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        if val != wire::XFLASH_SYNC_SIGNAL {
            return Err(EngineError::Protocol(format!(
                "DA SYNC 期望 0x{:08X}，实际 0x{:08X}",
                wire::XFLASH_SYNC_SIGNAL,
                val
            )));
        }
        Ok(())
    }

    /// DA extensions handshake (CUSTOM_ACK); returns the magic reported by the device.
    pub fn load_da_extensions(&mut self, port: &mut dyn StreamPort) -> Result<u32, EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, wire::XFLASH_DEVCTRL_ACK)?;
        Self::expect_status(port, "CUSTOM_ACK")?;
        let payload = Self::read_packet(port)?;
        let _ = Self::status(port);
        let magic = if payload.len() >= 4 {
            u32::from_le_bytes(payload[0..4].try_into().unwrap())
        } else {
            0
        };
        Ok(magic)
    }
}
