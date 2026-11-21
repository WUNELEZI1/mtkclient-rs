//! Transport layer abstraction.
//!
//! Defines only traits and semantics, not tied to a concrete backend:
//! - BROM: WinUSB (requires control transfers + bulk transfers)
//! - Preloader: serialport (primary path) + WinUSB (fallback)
//!
//! The existing `usb` / `preloader::transport` are concrete implementations; they will be gradually adapted to these two traits.
//! `mock` provides an in-memory pipe + fault injection for fixture replay and offline unit tests.

#[cfg(test)]
pub mod mock;
pub mod vdev;
pub mod wire;

use std::fmt;
use std::time::Duration;

/// Port kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum PortKind {
    /// BROM over WinUSB (VID 0x0E8D / PID 0x0003)
    BromUsb,
    /// Preloader over serial (VID 0x0E8D / PID 0x2000)
    PreloaderSerial,
    /// Preloader over WinUSB (fallback)
    PreloaderUsb,
    /// In-memory mock
    Mock,
}

#[cfg(test)]
impl PortKind {
    pub const fn is_brom(self) -> bool {
        matches!(self, PortKind::BromUsb)
    }
}

/// Transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum Direction {
    /// Device → host
    In,
    /// Host → device
    Out,
}

/// Endpoint number (without the direction bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub struct Endpoint(pub u8);

/// Control transfer request (the first four fields of a USB setup packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
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

#[cfg(test)]
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

/// Transport-layer error classification (retryable / non-retryable / needs user action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    /// Timeout, retryable
    Timeout,
    /// Device disconnected, user must replug
    Disconnected,
    #[cfg(test)]
    /// Operation not supported by the backend
    NotSupported(&'static str),
    #[cfg(test)]
    /// IO / driver error
    Io(String),
}

impl PortError {
    /// Whether automatic retry is worthwhile.
    pub const fn retryable(&self) -> bool {
        matches!(self, PortError::Timeout)
    }

    #[cfg(test)]
    /// Whether user action is required (replug / install driver).
    pub const fn needs_user(&self) -> bool {
        matches!(self, PortError::Disconnected)
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortError::Timeout => write!(f, "传输超时"),
            PortError::Disconnected => write!(f, "设备已断开"),
            #[cfg(test)]
            PortError::NotSupported(op) => write!(f, "后端不支持: {}", op),
            #[cfg(test)]
            PortError::Io(e) => write!(f, "IO 错误: {}", e),
        }
    }
}

impl std::error::Error for PortError {}

/// Bounded retry policy (for **transient** timeouts only, not disconnects).
///
/// Design trade-offs:
/// - Retry `Timeout` only. `Disconnected` (including "silent latch") must **fail immediately**,
///   letting the upper layer trigger checkpoint resume/replug prompts instead of waiting in place.
/// - Retry **only when the current read has not yet consumed any bytes**. Retrying after a partial protocol frame
///   would restart from the middle of the frame and misalign; in that case prefer an error (strict defense).
/// - Never retry writes: writes are not idempotent, and resending pollutes device-side state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries allowed per read transaction (0 = no retry).
    pub max_retries: u32,
    /// Initial backoff base (ms), then increasing as powers of 2.
    pub base_delay_ms: u64,
    /// Backoff cap (ms).
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // Default 3 retries, 10ms start, 100ms cap — enough for transient jitter without slowing normal flows.
        Self {
            max_retries: 3,
            base_delay_ms: 10,
            max_delay_ms: 100,
        }
    }
}

impl RetryPolicy {
    #[cfg(test)]
    /// Disable retry entirely (for strict mode / regression baselines).
    pub const fn none() -> Self {
        Self {
            max_retries: 0,
            base_delay_ms: 0,
            max_delay_ms: 0,
        }
    }

    /// Construct by retry count (using the default backoff).
    pub const fn with_max_retries(n: u32) -> Self {
        Self {
            max_retries: n,
            base_delay_ms: 10,
            max_delay_ms: 100,
        }
    }

    /// Backoff duration before retry `attempt` (1-based): exponential backoff + cap.
    pub fn delay(&self, attempt: u32) -> Duration {
        if self.base_delay_ms == 0 {
            return Duration::ZERO;
        }
        let shift = attempt.saturating_sub(1).min(6);
        let ms = (self.base_delay_ms << shift).min(self.max_delay_ms);
        Duration::from_millis(ms)
    }
}

/// Retrying port: wraps any `StreamPort` and provides bounded retries for `read_exact`.
///
/// Only a `Timeout` where "the read transaction has not consumed any bytes yet" is retried; all other errors propagate unchanged.
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

    #[cfg(test)]
    /// Cumulative retry count on this port (for diagnostics / test assertions).
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

    fn drain(&mut self) -> Result<(), PortError> {
        self.inner.drain()
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), PortError> {
        // Resume-style retry: read progress is kept in `buf`, so after a timeout only the **remaining** bytes are read.
        // This avoids losing a whole frame to a single hiccup, and avoids "starting over" and misaligning.
        let mut off = 0usize;
        let mut attempt: u32 = 0;
        while off < buf.len() {
            match self.inner.read(&mut buf[off..]) {
                Ok(0) => return Err(PortError::Disconnected),
                Ok(n) => {
                    off += n;
                    // Any progress resets the budget: the retry budget counts "consecutive no-progress" attempts.
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

/// Byte-stream port (implementable by both the Preloader serial port and BROM bulk endpoints).
pub trait StreamPort: Send {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError>;
    fn flush(&mut self) -> Result<(), PortError>;

    /// Discard unconsumed bytes on the input direction (used for resynchronization).
    ///
    /// Once a frame is truncated/misaligned, residual bytes pollute the next read; reliable recovery requires
    /// clearing the input buffer before resending an idempotent command (e.g. READ_DATA). Default is a no-op (unbuffered backends need no handling).
    fn drain(&mut self) -> Result<(), PortError> {
        Ok(())
    }

    /// Read exactly `buf`; return an error on timeout/disconnect. The default implementation is reused by all backends.
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

    /// Write all of `buf`.
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

#[cfg(test)]
/// BROM port: exposes control/bulk transfers on top of the byte stream.
///
/// Kamakiri2 depends on `control_transfer(0xA1, 0x25/0x21, ...)` and `(0x21, 0x20, ...)`,
/// so the BROM backend must implement this trait, not only `StreamPort`.
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

/// Enumeration diagnostics: explain "why it cannot connect" instead of just saying NotFound.
#[derive(Debug, Clone, Default)]
#[cfg(test)]
pub struct EnumerateDiagnostics {
    /// Driver-layer state (device manager view, valid on Windows)
    pub driver_state: Option<String>,
    /// Actually enumerated (VID, PID, SN, kind)
    pub found: Vec<(u16, u16, String, PortKind)>,
    /// Connection attempt records
    pub attempts: Vec<String>,
    /// Expected vs actual
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[cfg(test)]
impl EnumerateDiagnostics {
    pub fn push_attempt(&mut self, msg: impl Into<String>) {
        self.attempts.push(msg.into());
    }

    /// Human-readable report.
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
    fn port_kind_classification() {
        assert!(PortKind::BromUsb.is_brom());
        assert!(!PortKind::PreloaderSerial.is_brom());
        assert!(!PortKind::PreloaderUsb.is_brom());
        assert!(!PortKind::Mock.is_brom());
    }

    #[test]
    fn error_classification() {
        assert!(PortError::Timeout.retryable());
        assert!(PortError::Disconnected.needs_user());
        assert!(!PortError::NotSupported("x").retryable());
    }

    // ── Phase 1.1b: bounded retry ────────────────────────────────

    /// Programmable port: emits errors / bytes in scripted order, for precisely verifying retry semantics.
    struct FlakyPort {
        /// Remaining transient timeouts to raise
        timeouts_left: u32,
        /// Remaining disconnects to raise
        disconnects_left: u32,
        /// The first `partial_after_timeout` reads each return 1 byte before timing out (simulating "half a frame read")
        rx: VecDeque<u8>,
        /// Number of read calls made so far
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
        // 1st read returns 1 byte → 2nd transient timeout → 3rd fills the remaining 3 bytes.
        // Correct semantics is "resume": bytes already read are kept, never restart from scratch (would misalign).
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
        // After reading 1 byte the device keeps timing out: the budget counts "consecutive no-progress" attempts, so exhaustion must fail.
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
        let mut d = EnumerateDiagnostics {
            driver_state: Some("WinUSB 已安装".into()),
            ..Default::default()
        };
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
