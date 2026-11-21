//! 内存端口（mock）：内存 pipe + 故障注入。
//!
//! 用途：
//! - fixture 回放时作为被读取的一端；
//! - 协议状态机单测（第 N 字节错误 / 超时 / 断开）。

use std::collections::VecDeque;

use super::{BromPort, ControlRequest, Direction, Endpoint, PortError, StreamPort};

/// 故障类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// 读到 / 写到第 N 字节时返回 IO 错误
    Error,
    /// 第 N 字节处超时
    Timeout,
    /// 第 N 字节处断开
    Disconnect,
}

/// 单个注入点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// 触发位置（全局字节计数，从 0 开始）
    pub at: usize,
    pub kind: FaultKind,
}

/// 内存端口。
pub struct MockPort {
    /// 设备 → 主机（read 从这里取）
    rx: VecDeque<u8>,
    /// 主机 → 设备（write 写这里）
    tx: Vec<u8>,
    /// 已从 rx 读出的字节数（用于故障定位）
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

    /// 追加设备侧数据（可分段追加，模拟真实到达）。
    pub fn push_rx(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    /// 主机侧写入的字节。
    pub fn tx(&self) -> &[u8] {
        &self.tx
    }

    pub fn control_log(&self) -> &[(ControlRequest, usize)] {
        &self.control_log
    }

    pub fn bulk_log(&self) -> &[(Endpoint, Direction, usize)] {
        &self.bulk_log
    }

    /// 注入故障。
    pub fn with_fault(mut self, at: usize, kind: FaultKind) -> Self {
        self.faults.push(Fault { at, kind });
        self
    }

    /// 检查区间 `[from, to)` 是否命中故障；命中则消费它（只触发一次）。
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
        // 0x80 方向（IN）时从 rx 填充
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
        // 前 2 字节成功，后 2 字节无数据 → 超时
        assert_eq!(p.read_exact(&mut buf), Err(PortError::Timeout));
    }

    #[test]
    fn injected_fault_fires_once() {
        let mut p = MockPort::new(vec![0u8; 16]).with_fault(2, FaultKind::Error);
        // 第一次读的区间 [0,4) 覆盖故障点 2 → 立即报错
        let mut b = [0u8; 4];
        assert!(matches!(p.read(&mut b), Err(PortError::Io(_))));
        // 故障只触发一次
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
