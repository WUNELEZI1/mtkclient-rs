//! Engine commands dedicated to simulation / virtual devices.

use log::debug;

use crate::transport::StreamPort;
use crate::transport::wire;

use super::{DaEngine, EngineError};

impl DaEngine {
    /// Simulation only: run the exploit handshake against the device (real devices use each exploit's own implementation).
    pub fn sim_exploit(
        &mut self,
        port: &mut dyn StreamPort,
        name: &str,
    ) -> Result<(), EngineError> {
        let mut rp = self.retrying(port);
        let port: &mut dyn StreamPort = &mut rp;
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
