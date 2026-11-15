//! DA 协议引擎：完整状态机收敛到 [`StreamPort`]。
//!
//! 与 `DAXFlash` 的区别：
//! - `DAXFlash` 绑定 `Preloader`/`UsbDevice`，混入了大量 USB 特例（重连、端点、
//!   短包、`is_libusb` 分支），无法在 CI 里跑；
//! - 本模块只认 `&mut dyn StreamPort`，因此同一份代码可以跑在
//!   `transport::vdev`（虚拟设备）、`transport::mock`（录制回放）、
//!   或未来的真实串口/USB 适配器上。
//!
//! 覆盖范围（第一阶段）：连接（握手 / 芯片识别 / 安全标志）→ DA 上传 →
//! 存储打开（CUSTOM_SET_STORAGE）→ 读 flash。写/擦/RPMB 尚未迁移。
//!
//! 线协议与 `DAXFlash` 完全一致（命令码见 [`crate::transport::wire`]，并有
//! 一致性测试约束），因此"仿真里通过"不等于"虚构了另一套协议"。

use log::{debug, trace, warn};

use crate::transport::wire;
use crate::transport::{PortError, StreamPort};

/// 连接阶段的设备信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BromInfo {
    pub hw_code: u16,
    pub hw_sub: u16,
    /// target config 原始位（安全标志）
    pub target_config: u32,
}

/// 存储信息（来自 eMMC 信息查询）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageInfo {
    pub emmc_type: u32,
    pub block: u32,
    pub boot1: u64,
    pub boot2: u64,
    pub user: u64,
}

impl StorageInfo {
    pub fn type_name(&self) -> &'static str {
        match self.emmc_type {
            1 => "UFS",
            2 => "SD",
            3 => "MMC",
            6 => "UFS_CARD",
            _ => "eMMC",
        }
    }

    /// 作为 `CUSTOM_SET_STORAGE` 参数的类型值。
    pub fn storage_type(&self) -> u32 {
        self.emmc_type
    }
}

/// 协议阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 未连接
    Idle,
    /// 已握手，处于 BROM
    Brom,
    /// DA1 已运行（可发 XFlash 命令）
    Da1,
    /// DA2 已加载 + 存储已打开
    Ready,
}

/// 引擎错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// 端口层错误
    Port(PortError),
    /// 协议不符合预期（收到非法 magic / 长度 / 状态码）
    Protocol(String),
    /// DA 被 SLA 拒绝
    SlaRequired,
    /// 步骤顺序错误（例如未连 DA 就发 READ_DATA）
    OutOfOrder { phase: Phase, want: &'static str },
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Port(e) => write!(f, "端口错误: {}", e),
            EngineError::Protocol(m) => write!(f, "协议错误: {}", m),
            EngineError::SlaRequired => write!(f, "DA 被 SLA 拒绝（需先绕过安全启动）"),
            EngineError::OutOfOrder { phase, want } => {
                write!(f, "阶段错误: 当前 {:?}，需要 {}", phase, want)
            }
        }
    }
}

impl std::error::Error for EngineError {}

impl From<PortError> for EngineError {
    fn from(e: PortError) -> Self {
        EngineError::Port(e)
    }
}

/// DA1 在 SRAM 中的加载地址（MTK 惯例）。
pub const DA1_SRAM_ADDR: u32 = 0x0020_1000;

/// 协议引擎。
pub struct DaEngine {
    phase: Phase,
    info: Option<BromInfo>,
    storage: Option<StorageInfo>,
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
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// 连接阶段拿到的芯片信息（外部工具 / 诊断用）。
    #[allow(dead_code)]
    pub fn info(&self) -> Option<BromInfo> {
        self.info
    }

    /// 已查询到的存储信息。
    #[allow(dead_code)]
    pub fn storage(&self) -> Option<StorageInfo> {
        self.storage
    }

    // ── 底层读写（统一带 trace hex dump） ─────────────────────

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

    /// 1 字节命令 + 回显校验。
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

    /// 4 字节 BE 参数 + 回显校验。
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

    // ── 阶段 1：BROM 连接 ───────────────────────────────────

    /// 握手 + 芯片识别 + 安全标志读取。
    pub fn connect(&mut self, port: &mut dyn StreamPort) -> Result<BromInfo, EngineError> {
        // 握手：逐字节发送，设备回按位取反
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

        // BROM sync（FE）：设备回 1 字节，不校验具体值
        Self::wr(port, &[0xFE])?;
        let _ = Self::rd_vec(port, 1)?;

        // 芯片识别
        Self::echo1(port, wire::BROM_GET_HW_CODE)?;
        let hw = Self::rd_vec(port, 4)?;
        let hw_code = u16::from_be_bytes([hw[0], hw[1]]);
        let hw_sub = u16::from_be_bytes([hw[2], hw[3]]);

        // 安全标志
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

    // ── 阶段 2：DA 上传 ─────────────────────────────────────

    /// 上传 DA1 并跳转，等待 DA 的 XFlash SYNC。
    pub fn load_da(&mut self, port: &mut dyn StreamPort, da: &[u8]) -> Result<(), EngineError> {
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
        // 数据体（按 64 KiB 分块，模拟真实分片；设备侧按单字节累积）
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
        // 等 0xC0（容忍前置 0x00 填充）
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

    /// 等待并消费 DA 的 SYNC 信号。
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

    // ── XFlash 基础原语 ────────────────────────────────────

    fn send_cmd(port: &mut dyn StreamPort, cmd: u32) -> Result<(), EngineError> {
        let mut pkt = [0u8; 12];
        pkt[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        pkt[4..8].copy_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        pkt[8..12].copy_from_slice(&4u32.to_le_bytes());
        Self::wr(port, &pkt)?;
        Self::wr(port, &cmd.to_le_bytes())
    }

    fn send_payload(port: &mut dyn StreamPort, payload: &[u8]) -> Result<(), EngineError> {
        let mut pkt = [0u8; 12];
        pkt[0..4].copy_from_slice(&wire::XFLASH_MAGIC.to_le_bytes());
        pkt[4..8].copy_from_slice(&wire::XFLASH_TYPE_FLOW.to_le_bytes());
        pkt[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        Self::wr(port, &pkt)?;
        Self::wr(port, payload)
    }

    /// 读一个流程包，跳过设备异步消息包。
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

    /// devctrl 无参命令。
    #[allow(dead_code)]
    pub fn devctrl(port: &mut dyn StreamPort, cmd: u32) -> Result<(), EngineError> {
        Self::send_cmd(port, wire::XFLASH_CMD_DEVICE_CTRL)?;
        Self::expect_status(port, "DEVICE_CTRL")?;
        Self::send_cmd(port, cmd)?;
        Self::expect_status(port, "devctrl cmd")?;
        Ok(())
    }

    /// devctrl 带参命令。
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

    /// 读 flash（单次 READ_DATA，长度受包长限制；调用方自行分块）。
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
        Self::send_cmd(port, wire::XFLASH_CMD_READ_DATA)?;
        Self::expect_status(port, "READ_DATA cmd")?;

        let mut param = [0u8; wire::XFLASH_READ_PARAM_LEN];
        param[..4].copy_from_slice(&1u32.to_le_bytes());
        param[4..8].copy_from_slice(&8u32.to_le_bytes()); // parttype=8 物理分区
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

    // ── 阶段 3：打开存储 ────────────────────────────────────

    /// 查询存储信息（eMMC / UFS 容量与块大小）。
    pub fn query_storage(&mut self, port: &mut dyn StreamPort) -> Result<StorageInfo, EngineError> {
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
        let rd = |o: usize| u64::from_le_bytes(payload[o..o + 8].try_into().unwrap());
        let info = StorageInfo {
            emmc_type: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            block: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            boot1: rd(8),
            boot2: rd(16),
            user: rd(32),
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

    /// 打开存储：CUSTOM_SET_STORAGE（DA extensions 必须已加载才可用）。
    pub fn open_storage(&mut self, port: &mut dyn StreamPort) -> Result<(), EngineError> {
        // 存储信息查询是"尽力而为"：老 DA / 定制 DA 可能不支持 0x00040001，
        // 但只要 READ_DATA 能用，printgpt 依然应该成功（真机 cmd_printgpt 同此行为）。
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
            // 真机上 DA extensions 未加载时会走到这里：存储仍可读，仅扩展功能受限
            warn!("[engine] CUSTOM_SET_STORAGE 失败（可忽略）: {}", e);
        }
        self.phase = Phase::Ready;
        Ok(())
    }

    /// DA extensions 握手（CUSTOM_ACK），返回设备返回的 magic。
    pub fn load_da_extensions(&mut self, port: &mut dyn StreamPort) -> Result<u32, EngineError> {
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

    /// 仿真专用：向设备发起 exploit 握手（真机走各 exploit 自身实现）。
    pub fn sim_exploit(
        &mut self,
        port: &mut dyn StreamPort,
        name: &str,
    ) -> Result<(), EngineError> {
        let id = crate::transport::vdev::exploit_id(name)
            .ok_or_else(|| EngineError::Protocol(format!("仿真设备不认识 exploit: {}", name)))?;
        Self::wr(port, &[wire::SIM_EXPLOIT_CMD, id])?;
        let resp = Self::rd_vec(port, 3)?;
        if resp[0] != wire::SIM_EXPLOIT_CMD || resp[1] != id {
            return Err(EngineError::Protocol("exploit 握手回包不匹配".into()));
        }
        if resp[2] != 0 {
            return Err(EngineError::Protocol(format!(
                "设备拒绝了 exploit {}（该芯片不适用）",
                name
            )));
        }
        debug!("[engine] exploit {} 已被设备接受", name);
        Ok(())
    }
}

/// 生成 trace 级 hex dump（16 字节一行，带偏移与可打印字符）。
pub fn hexdump(data: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        out.push_str(&format!("  {:08X}  ", i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => out.push_str(&format!("{:02X} ", b)),
                None => out.push_str("   "),
            }
        }
        out.push(' ');
        out.push('|');
        for b in chunk {
            out.push(if (0x20..0x7F).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::vdev::VirtualDevice;

    fn da_bytes() -> Vec<u8> {
        // 256 字节假 DA（含 MTK_DA_v5 标识，供协议版本判定使用）
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
    fn hexdump_layout() {
        let s = hexdump(b"AB");
        assert!(s.contains("00000000  41 42 "));
        assert!(s.contains("|AB|"));
    }
}
