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

#[cfg(test)]
mod diagnostics;
mod error;
mod retry;

pub use error::PortError;
pub use retry::{RetryPolicy, RetryingPort};

#[cfg(test)]
pub use diagnostics::EnumerateDiagnostics;

#[cfg(test)]
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
