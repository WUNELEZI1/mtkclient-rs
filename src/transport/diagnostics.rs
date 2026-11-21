use super::PortKind;

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
