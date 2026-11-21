//! In-memory port (mock): memory pipe + fault injection.
//!
//! Use cases:
//! - act as the read side during fixture replay;
//! - protocol state machine unit tests (error at byte N / timeout / disconnect).

use std::collections::VecDeque;

use super::{BromPort, ControlRequest, Direction, Endpoint, PortError, StreamPort};

/// Fault kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// Return an IO error when reading / writing byte N
    Error,
    /// Timeout at byte N
    Timeout,
    /// Disconnect at byte N
    Disconnect,
}

/// A single injection point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// Trigger position (global byte counter, 0-based)
    pub at: usize,
    pub kind: FaultKind,
}

/// In-memory port.
pub struct MockPort {
    /// Device → host (reads take from here)
    rx: VecDeque<u8>,
    /// Host → device (writes go here)
    tx: Vec<u8>,
    /// Bytes read from rx so far (used to locate faults)
    read_pos: usize,
    written_pos: usize,
    faults: Vec<Fault>,
    control_log: Vec<(ControlRequest, usize)>,
    bulk_log: Vec<(Endpoint, Direction, usize)>,
}

impl MockPort {
    pub fn new(rx: impl Into<Vec<u8>>) -> Self {
        Self {
            rx: rx.into().into_iter().collect(),
            tx: Vec::new(),
            read_pos: 0,
            written_pos: 0,
            faults: Vec::new(),
            control_log: Vec::new(),
            bulk_log: Vec::new(),
        }
    }

    /// Append device-side data (can be appended in chunks to mimic real arrival).
    pub fn push_rx(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    /// Bytes written by the host.
    pub fn tx(&self) -> &[u8] {
        &self.tx
    }

    pub fn control_log(&self) -> &[(ControlRequest, usize)] {
        &self.control_log
    }

    pub fn bulk_log(&self) -> &[(Endpoint, Direction, usize)] {
        &self.bulk_log
    }

    /// Inject a fault.
    pub fn with_fault(mut self, at: usize, kind: FaultKind) -> Self {
        self.faults.push(Fault { at, kind });
        self
    }

    /// Check whether range `[from, to)` hits a fault; consume it on hit (fires only once).
    fn hit_fault(&mut self, from: usize, to: usize) -> Option<PortError> {
        let idx = self.faults.iter().position(|f| f.at >= from && f.at < to)?;
        let f = self.faults.remove(idx);
        Some(match f.kind {
            FaultKind::Error => PortError::Io("mock: 注入的 IO 错误".into()),
            FaultKind::Timeout => PortError::Timeout,
            FaultKind::Disconnect => PortError::Disconnected,
        })
    }
}

impl StreamPort for MockPort {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PortError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(e) = self.hit_fault(self.read_pos, self.read_pos + buf.len()) {
            return Err(e);
        }
        if self.rx.is_empty() {
            return Err(PortError::Timeout);
        }
        let n = buf.len().min(self.rx.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.rx.pop_front().unwrap();
        }
        self.read_pos += n;
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, PortError> {
        if let Some(e) = self.hit_fault(self.written_pos, self.written_pos + buf.len()) {
            return Err(e);
        }
        self.tx.extend_from_slice(buf);
        self.written_pos += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), PortError> {
        Ok(())
    }
}

impl BromPort for MockPort {
    fn control_transfer(
        &mut self,
        req: ControlRequest,
        data: &mut [u8],
    ) -> Result<usize, PortError> {
        self.control_log.push((req, data.len()));
        // For 0x80 direction (IN), fill from rx
        if req.request_type & 0x80 != 0 {
            return self.read(data);
        }
        self.write(data)
    }

    fn bulk_transfer(
        &mut self,
        ep: Endpoint,
        data: &mut [u8],
        dir: Direction,
    ) -> Result<usize, PortError> {
        self.bulk_log.push((ep, dir, data.len()));
        match dir {
            Direction::In => self.read(data),
            Direction::Out => self.write(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_roundtrip() {
        let mut p = MockPort::new(vec![1u8, 2, 3, 4]);
        let mut buf = [0u8; 2];
        assert_eq!(p.read(&mut buf).unwrap(), 2);
        assert_eq!(buf, [1, 2]);
        p.write_all(b"hi").unwrap();
        assert_eq!(p.tx(), b"hi");
    }

    #[test]
    fn read_exact_detects_disconnect() {
        let mut p = MockPort::new(vec![1u8, 2]);
        let mut buf = [0u8; 4];
        // First 2 bytes succeed, next 2 have no data → timeout
        assert_eq!(p.read_exact(&mut buf), Err(PortError::Timeout));
    }

    #[test]
    fn injected_fault_fires_once() {
        let mut p = MockPort::new(vec![0u8; 16]).with_fault(2, FaultKind::Error);
        // First read covers range [0,4) including fault point 2 → immediate error
        let mut b = [0u8; 4];
        assert!(matches!(p.read(&mut b), Err(PortError::Io(_))));
        // Fault fires only once
        let mut b2 = [0u8; 4];
        assert!(p.read(&mut b2).is_ok());
    }

    #[test]
    fn fault_outside_interval_does_not_fire() {
        let mut p = MockPort::new(vec![0u8; 16]).with_fault(8, FaultKind::Timeout);
        let mut b = [0u8; 4];
        assert!(p.read(&mut b).is_ok());
        assert!(p.read(&mut b).is_ok());
        let mut b2 = [0u8; 4];
        assert_eq!(p.read(&mut b2), Err(PortError::Timeout));
    }

    #[test]
    fn bulk_transfer_logs_endpoint_and_direction() {
        let mut p = MockPort::new(b"in".to_vec());
        let ep = Endpoint(0x81);
        let mut b = [0u8; 2];
        assert_eq!(p.bulk_transfer(ep, &mut b, Direction::In).unwrap(), 2);
        assert_eq!(&b, b"in");
        let mut out = *b"out";
        assert_eq!(
            p.bulk_transfer(Endpoint(0x01), &mut out, Direction::Out)
                .unwrap(),
            3
        );
        assert_eq!(p.tx(), b"out");
        let log = p.bulk_log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, ep);
        assert_eq!(log[0].1, Direction::In);
        assert_eq!(log[1].1, Direction::Out);
    }

    #[test]
    fn injected_disconnect_maps_to_port_error() {
        let mut p = MockPort::new(vec![0u8; 16]).with_fault(0, FaultKind::Disconnect);
        let mut b = [0u8; 4];
        assert_eq!(p.read(&mut b), Err(PortError::Disconnected));
    }

    #[test]
    fn brom_control_in_reads_from_rx() {
        let mut p = MockPort::new(b"linecode".to_vec());
        let mut data = [0u8; 8];
        let n = p
            .control_transfer(ControlRequest::new(0xA1, 0x25, 0, 0), &mut data)
            .unwrap();
        assert_eq!(n, 8);
        assert_eq!(&data, b"linecode");
        assert_eq!(p.control_log().len(), 1);
    }
}
