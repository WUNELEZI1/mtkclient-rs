use std::time::Duration;

use super::{PortError, StreamPort};

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
