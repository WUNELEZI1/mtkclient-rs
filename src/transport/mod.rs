//! 传输层抽象。
//!
//! 只定义 trait 与语义，不绑定具体后端：
//! - BROM：WinUSB（需控制传输 + 批量传输）
//! - Preloader：serialport（主路径）+ WinUSB（兜底）
//!
//! 现有 `usb` / `preloader::transport` 为具体实现；后续逐步适配到这两个 trait。
//! `mock` 提供内存 pipe + 故障注入，供 fixture 回放与单测脱机运行。

pub mod mock;
pub mod vdev;
pub mod wire;

use std::fmt;

/// 端口类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    /// BROM over WinUSB（VID 0x0E8D / PID 0x0003）
    BromUsb,
    /// Preloader over serial（VID 0x0E8D / PID 0x2000）
    PreloaderSerial,
    /// Preloader over WinUSB（兜底）
    PreloaderUsb,
    /// 内存 mock
    Mock,
}

impl PortKind {
    pub const fn is_brom(self) -> bool {
        matches!(self, PortKind::BromUsb)
    }
}

/// 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 设备 → 主机
    In,
    /// 主机 → 设备
    Out,
}

/// 端点号（不含方向位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint(pub u8);

/// 控制传输请求（对应 USB setup packet 的前四项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRequest {
    /// bmRequestType
    pub request_type: u8,
    /// bRequest
    pub request: u8,
    /// wValue
    pub value: u16,
    /// wIndex
    pub index: u16,
}

impl ControlRequest {
    pub const fn new(request_type: u8, request: u8, value: u16, index: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
        }
    }
}

/// 传输层错误分类（可重试 / 不可重试 / 需用户介入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// 超时，可重试
    Timeout,
    /// 设备断开，需用户重插
    Disconnected,
    /// 后端不支持该操作
    NotSupported(&'static str),
    /// IO / 驱动错误
    Io(String),
}

impl PortError {
    /// 是否值得自动重试。
    pub const fn retryable(&self) -> bool {
        matches!(self, PortError::Timeout)
    }

    /// 是否需要用户介入（重插 / 装驱动）。
    pub const fn needs_user(&self) -> bool {
        matches!(self, PortError::Disconnected)
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortError::Timeout => write!(f, "传输超时"),
            PortError::Disconnected => write!(f, "设备已断开"),
            PortError::NotSupported(op) => write!(f, "后端不支持: {}", op),
            PortError::Io(e) => write!(f, "IO 错误: {}", e),
        }
    }
}

impl std::error::Error for PortError {}

/// 字节流端口（Preloader 串口 / BROM 批量端点均可实现）。
pub trait StreamPort: Send {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError>;
    fn flush(&mut self) -> Result<(), PortError>;
    fn kind(&self) -> PortKind;

    /// 读满 `buf`，超时/断开即返回错误。默认实现供各后端复用。
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), PortError> {
        let mut off = 0;
        while off < buf.len() {
            let n = self.read(&mut buf[off..])?;
            if n == 0 {
                return Err(PortError::Disconnected);
            }
            off += n;
        }
        Ok(())
    }

    /// 写全 `buf`。
    fn write_all(&mut self, buf: &[u8]) -> Result<(), PortError> {
        let mut off = 0;
        while off < buf.len() {
            let n = self.write(&buf[off..])?;
            if n == 0 {
                return Err(PortError::Disconnected);
            }
            off += n;
        }
        Ok(())
    }
}

/// BROM 端口：在字节流之上暴露控制传输 / 批量传输。
///
/// Kamakiri2 依赖 `control_transfer(0xA1, 0x25/0x21, ...)` 与 `(0x21, 0x20, ...)`，
/// 因此 BROM 后端必须实现本 trait，不能只实现 `StreamPort`。
pub trait BromPort: StreamPort {
    fn control_transfer(
        &mut self,
        req: ControlRequest,
        data: &mut [u8],
    ) -> Result<usize, PortError>;

    fn bulk_transfer(
        &mut self,
        ep: Endpoint,
        data: &mut [u8],
        dir: Direction,
    ) -> Result<usize, PortError>;
}

/// 枚举诊断：把“为什么连不上”讲清楚，而不是只说 NotFound。
#[derive(Debug, Clone, Default)]
pub struct EnumerateDiagnostics {
    /// 驱动层状态（设备管理器视角，Windows 有效）
    pub driver_state: Option<String>,
    /// 实际枚举到的 (VID, PID, SN, kind)
    pub found: Vec<(u16, u16, String, PortKind)>,
    /// 连接尝试记录
    pub attempts: Vec<String>,
    /// 期望 vs 实际
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl EnumerateDiagnostics {
    pub fn push_attempt(&mut self, msg: impl Into<String>) {
        self.attempts.push(msg.into());
    }

    /// 人类可读报告。
    pub fn report(&self) -> String {
        let mut out = String::new();
        if let Some(d) = &self.driver_state {
            out.push_str(&format!("驱动状态: {}\n", d));
        }
        if self.found.is_empty() {
            out.push_str("枚举结果: 未发现 MTK 设备\n");
        } else {
            for (vid, pid, sn, kind) in &self.found {
                out.push_str(&format!(
                    "枚举结果: {:04X}:{:04X} sn={} kind={:?}\n",
                    vid, pid, sn, kind
                ));
            }
        }
        if let Some(e) = &self.expected {
            out.push_str(&format!("期望: {}\n", e));
        }
        if let Some(a) = &self.actual {
            out.push_str(&format!("实际: {}\n", a));
        }
        for a in &self.attempts {
            out.push_str(&format!("尝试: {}\n", a));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_classification() {
        assert!(PortError::Timeout.retryable());
        assert!(PortError::Disconnected.needs_user());
        assert!(!PortError::NotSupported("x").retryable());
    }

    #[test]
    fn diagnostics_report() {
        let mut d = EnumerateDiagnostics::default();
        d.driver_state = Some("WinUSB 已安装".into());
        d.found
            .push((0x0E8D, 0x0003, "SN1234".into(), PortKind::BromUsb));
        d.expected = Some("BROM".into());
        d.actual = Some("Preloader".into());
        d.push_attempt("打开 0E8D:0003");
        let r = d.report();
        assert!(r.contains("WinUSB 已安装"));
        assert!(r.contains("0E8D:0003"));
        assert!(r.contains("期望: BROM"));
        assert!(r.contains("尝试: 打开"));
    }
}
