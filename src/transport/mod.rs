//! 传输层抽象。
//!
//! 只定义 trait 与语义，不绑定具体后端：
//! - BROM：WinUSB（需控制传输 + 批量传输）
//! - Preloader：serialport（主路径）+ WinUSB（兜底）
//!
//! 现有 `usb` / `preloader::transport` 为具体实现；后续逐步适配到这两个 trait。
//! `mock` 提供内存 pipe + 故障注入，供 fixture 回放与单测脱机运行。

#[cfg(test)]
pub mod mock;
pub mod vdev;
pub mod wire;

use std::fmt;
use std::time::Duration;

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

    #[cfg(test)]
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

/// 有界重试策略（只针对**瞬时**超时，不针对断开）。
///
/// 设计取舍：
/// - 只重试 `Timeout`。`Disconnected`（含"静默锁存"）必须**立刻失败**，
///   交给上层触发断点续传/提示重插，绝不原地死等。
/// - **只在本次读尚未消费任何字节时**才重试。协议帧一旦读了半截再重试，
///   会从帧中间重新开始而错位；此时宁可报错（绝对防御）。
/// - 写方向不重试：写不是幂等操作，重发会污染设备侧状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 单次读事务允许的重试次数（0 = 不重试）。
    pub max_retries: u32,
    /// 首次退避基数（毫秒），之后按 2 的幂递增。
    pub base_delay_ms: u64,
    /// 退避上限（毫秒）。
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // 默认 3 次、10ms 起步、封顶 100ms —— 对瞬时抖动足够，又不拖慢正常流程。
        Self {
            max_retries: 3,
            base_delay_ms: 10,
            max_delay_ms: 100,
        }
    }
}

impl RetryPolicy {
    /// 完全关闭重试（用于严格模式 / 回归对照）。
    pub const fn none() -> Self {
        Self {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
        }
    }

    /// 按次数构造（沿用默认退避）。
    pub const fn with_max_retries(n: u32) -> Self {
        Self {
            max_retries: n,
            base_delay_ms: 10,
            max_delay_ms: 100,
        }
    }

    /// 第 `attempt` 次重试（从 1 开始）前的退避时长：指数退避 + 封顶。
    pub fn delay(&self, attempt: u32) -> Duration {
        if self.base_delay_ms == 0 {
            return Duration::ZERO;
        }
        let shift = attempt.saturating_sub(1).min(6);
        let ms = (self.base_delay_ms << shift).min(self.max_delay_ms);
        Duration::from_millis(ms)
    }
}

/// 重试端口：包裹任意 `StreamPort`，为 `read_exact` 提供有界重试。
///
/// 只有"读事务尚未消费任何字节"的 `Timeout` 会被重试；其余错误原样上抛。
pub struct RetryingPort<'a> {
    inner: &'a mut dyn StreamPort,
    policy: RetryPolicy,
    retries: u32,
}

impl<'a> RetryingPort<'a> {
    pub fn new(inner: &'a mut dyn StreamPort, policy: RetryPolicy) -> Self {
        Self {
            inner,
            policy,
            retries: 0,
        }
    }

    /// 本端口上累计发生的重试次数（诊断 / 测试断言用）。
    pub fn retries(&self) -> u32 {
        self.retries
    }
}

impl StreamPort for RetryingPort<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
        self.inner.read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> Result<(), PortError> {
        self.inner.flush()
    }

    fn kind(&self) -> PortKind {
        self.inner.kind()
    }

    fn drain(&mut self) -> Result<(), PortError> {
        self.inner.drain()
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), PortError> {
        // 续读式重试：已读进度保留在 `buf` 中，超时后只补读**剩余**字节。
        // 这样既不会因为一次抖动丢掉整帧，也不会"从头重来"导致错位。
        let mut off = 0usize;
        let mut attempt: u32 = 0;
        while off < buf.len() {
            match self.inner.read(&mut buf[off..]) {
                Ok(0) => return Err(PortError::Disconnected),
                Ok(n) => {
                    off += n;
                    // 有进展即重置预算：重试预算衡量的是"连续无进展"次数。
                    attempt = 0;
                }
                Err(e) => {
                    if !e.retryable() || attempt >= self.policy.max_retries {
                        log::debug!(
                            "[retry] 放弃读取：{:?}（已读 {}/{} 字节，重试 {} 次）",
                            e,
                            off,
                            buf.len(),
                            attempt
                        );
                        return Err(e);
                    }
                    attempt += 1;
                    self.retries += 1;
                    let d = self.policy.delay(attempt);
                    log::debug!(
                        "[retry] 瞬时超时（已读 {}/{}），第 {}/{} 次重试，退避 {}ms",
                        off,
                        buf.len(),
                        attempt,
                        self.policy.max_retries,
                        d.as_millis()
                    );
                    if !d.is_zero() {
                        std::thread::sleep(d);
                    }
                }
            }
        }
        Ok(())
    }
}

/// 字节流端口（Preloader 串口 / BROM 批量端点均可实现）。
pub trait StreamPort: Send {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError>;
    fn flush(&mut self) -> Result<(), PortError>;
    fn kind(&self) -> PortKind;

    /// 丢弃输入方向上尚未消费的字节（重新同步用）。
    ///
    /// 一帧被截断/错位后，残余字节会污染下一次读；只有清空输入缓冲再重发
    /// 幂等命令（如 READ_DATA）才能可靠恢复。默认空实现（无缓冲的后端无需处理）。
    fn drain(&mut self) -> Result<(), PortError> {
        Ok(())
    }

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
    use std::collections::VecDeque;

    #[test]
    fn error_classification() {
        assert!(PortError::Timeout.retryable());
        assert!(PortError::Disconnected.needs_user());
        assert!(!PortError::NotSupported("x").retryable());
    }

    // ── Phase 1.1b：有界重试 ────────────────────────────────

    /// 可编程端口：按脚本依次吐出错误 / 字节，用于精确验证重试语义。
    struct FlakyPort {
        /// 剩余要抛出的瞬时超时次数
        timeouts_left: u32,
        /// 剩余要抛出的断开次数
        disconnects_left: u32,
        /// 前 `partial_after_timeout` 次 read 各返回 1 字节后再超时（模拟"帧读到一半"）
        rx: VecDeque<u8>,
        /// 已发生的 read 调用次数
        reads: u32,
    }

    impl FlakyPort {
        fn new(rx: impl Into<Vec<u8>>) -> Self {
            Self {
                timeouts_left: 0,
                disconnects_left: 0,
                rx: rx.into().into_iter().collect(),
                reads: 0,
            }
        }
    }

    impl StreamPort for FlakyPort {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
            self.reads += 1;
            if self.disconnects_left > 0 {
                self.disconnects_left -= 1;
                return Err(PortError::Disconnected);
            }
            if self.timeouts_left > 0 {
                self.timeouts_left -= 1;
                return Err(PortError::Timeout);
            }
            if buf.is_empty() {
                return Ok(0);
            }
            if self.rx.is_empty() {
                return Err(PortError::Timeout);
            }
            let n = buf.len().min(self.rx.len());
            for slot in buf.iter_mut().take(n) {
                *slot = self.rx.pop_front().unwrap();
            }
            Ok(n)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
            if self.timeouts_left > 0 {
                self.timeouts_left -= 1;
                return Err(PortError::Timeout);
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> Result<(), PortError> {
            Ok(())
        }

        fn kind(&self) -> PortKind {
            PortKind::Mock
        }
    }

    #[test]
    fn retry_recovers_transient_timeouts() {
        let mut port = FlakyPort::new([1u8, 2, 3, 4]);
        port.timeouts_left = 2;
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(3));
        let mut buf = [0u8; 4];
        rp.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);
        assert_eq!(rp.retries(), 2, "两次瞬时超时都应被重试掉");
    }

    #[test]
    fn retry_is_bounded_and_reports_timeout() {
        let mut port = FlakyPort::new([1u8, 2]);
        port.timeouts_left = 10;
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(3));
        let mut buf = [0u8; 2];
        assert_eq!(rp.read_exact(&mut buf), Err(PortError::Timeout));
        assert_eq!(rp.retries(), 3, "重试次数必须被上限截断");
    }

    #[test]
    fn retry_resumes_after_partial_read() {
        // 第 1 次读 1 字节 → 第 2 次瞬时超时 → 第 3 次补齐剩余 3 字节。
        // 正确语义是"续读"：已读字节保留，绝不能从头重来（会错位）。
        struct HalfPort {
            step: u32,
        }
        impl StreamPort for HalfPort {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
                self.step += 1;
                match self.step {
                    1 => {
                        buf[0] = 0xAA;
                        Ok(1)
                    }
                    2 => Err(PortError::Timeout),
                    _ => {
                        let n = buf.len().min(3);
                        for slot in buf.iter_mut().take(n) {
                            *slot = 0xBB;
                        }
                        Ok(n)
                    }
                }
            }
            fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> Result<(), PortError> {
                Ok(())
            }
            fn kind(&self) -> PortKind {
                PortKind::Mock
            }
        }
        let mut port = HalfPort { step: 0 };
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(3));
        let mut buf = [0u8; 4];
        rp.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [0xAA, 0xBB, 0xBB, 0xBB], "已读字节必须保留，续读补齐");
        assert_eq!(rp.retries(), 1);
    }

    #[test]
    fn partial_read_then_persistent_timeout_fails() {
        // 读到 1 字节后设备持续超时：预算是"连续无进展"次数，耗尽必须失败。
        struct StuckPort {
            first: bool,
        }
        impl StreamPort for StuckPort {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
                if self.first {
                    self.first = false;
                    buf[0] = 0x11;
                    return Ok(1);
                }
                Err(PortError::Timeout)
            }
            fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> Result<(), PortError> {
                Ok(())
            }
            fn kind(&self) -> PortKind {
                PortKind::Mock
            }
        }
        let mut port = StuckPort { first: true };
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(2));
        let mut buf = [0u8; 4];
        assert_eq!(rp.read_exact(&mut buf), Err(PortError::Timeout));
        assert_eq!(rp.retries(), 2, "连续无进展的重试必须被上限截断");
    }

    #[test]
    fn disconnected_is_never_retried() {
        let mut port = FlakyPort::new([1u8]);
        port.disconnects_left = 1;
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(5));
        let mut buf = [0u8; 1];
        assert_eq!(rp.read_exact(&mut buf), Err(PortError::Disconnected));
        assert_eq!(rp.retries(), 0, "断开必须立刻失败，触发续传而非原地重试");
    }

    #[test]
    fn none_policy_never_retries() {
        let mut port = FlakyPort::new([1u8]);
        port.timeouts_left = 1;
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::none());
        let mut buf = [0u8; 1];
        assert_eq!(rp.read_exact(&mut buf), Err(PortError::Timeout));
        assert_eq!(rp.retries(), 0);
    }

    #[test]
    fn write_is_never_retried() {
        let mut port = FlakyPort::new(Vec::new());
        port.timeouts_left = 1;
        let mut rp = RetryingPort::new(&mut port, RetryPolicy::with_max_retries(5));
        assert_eq!(rp.write_all(&[0x01]), Err(PortError::Timeout));
        assert_eq!(rp.retries(), 0, "写非幂等，不得重试");
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let p = RetryPolicy {
            max_retries: 5,
            base_delay_ms: 10,
            max_delay_ms: 50,
        };
        assert_eq!(p.delay(1), Duration::from_millis(10));
        assert_eq!(p.delay(2), Duration::from_millis(20));
        assert_eq!(p.delay(3), Duration::from_millis(40));
        assert_eq!(p.delay(4), Duration::from_millis(50), "必须封顶");
        assert_eq!(p.delay(99), Duration::from_millis(50));
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
