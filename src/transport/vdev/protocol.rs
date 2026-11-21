use super::wire;
use super::{DeviceFault, PortError, SIM_EXPLOITS, State, VirtualDevice, WriteSession};

impl VirtualDevice {
    /// Advance the state machine by one byte.
    pub(super) fn feed(&mut self, b: u8) -> Result<(), PortError> {
        self.host_bytes += 1;
        self.written.push(b);
        if self.dead {
            return Err(PortError::Disconnected);
        }
        // Fault injection: go silent / disconnect at the trigger point
        if self
            .faults
            .iter()
            .any(|(at, kind)| *at == self.host_bytes && *kind == DeviceFault::Dead)
        {
            self.dead = true;
            self.ev(format!("fault: dead at host byte {}", self.host_bytes));
            return Err(PortError::Disconnected);
        }
        if self
            .faults
            .iter()
            .any(|(at, kind)| *at == self.host_bytes && *kind == DeviceFault::Silent)
        {
            self.silent = true;
            self.ev(format!("fault: silent at host byte {}", self.host_bytes));
            return Ok(());
        }
        if self.silent {
            return Ok(());
        }

        let state = std::mem::replace(&mut self.state, State::Brom);
        self.state = match state {
            State::Handshake(i) => {
                let want = wire::BROM_HANDSHAKE[i];
                if b != want {
                    self.ev(format!("handshake: 期望 0x{:02X} 收到 0x{:02X}", want, b));
                    // Echo a wrong byte so the host retries
                    self.push(&[!b]);
                    State::Handshake(0)
                } else {
                    self.push(&[!b]);
                    if i + 1 == wire::BROM_HANDSHAKE.len() {
                        self.ev("handshake: ok");
                        State::Brom
                    } else {
                        State::Handshake(i + 1)
                    }
                }
            }
            State::Brom => {
                // BROM sync FE does not echo the command itself, only a status byte
                if b == 0xFE {
                    self.push(&[wire::BROM_SYNC_FE_ACK]);
                    State::Brom
                } else if b == wire::SIM_EXPLOIT_CMD {
                    // Sim-only handshake: the command itself is not echoed; response is [cmd, id, status]
                    State::ExploitId
                } else if b == 0x00 {
                    // Possibly a 4-byte word command (BE, high byte 0)
                    State::BromWord {
                        buf: [0, 0, 0, 0],
                        n: 1,
                    }
                } else {
                    self.on_brom_cmd(b)?
                }
            }
            State::BromWord { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::BromWord { buf, n: n + 1 }
                } else {
                    let word = u32::from_be_bytes(buf);
                    // Echo the whole word
                    self.push(&buf);
                    let cmd = (word & 0xFF) as u8;
                    self.on_brom_cmd_after_echo(cmd)?
                }
            }
            State::SendDaArgs { idx, mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::SendDaArgs { idx, buf, n: n + 1 }
                } else {
                    // Echo the argument
                    self.push(&buf);
                    let val = u32::from_be_bytes(buf);
                    match idx {
                        0 => {
                            self.send_addr = val;
                            State::SendDaArgs {
                                idx: 1,
                                buf: [0; 4],
                                n: 0,
                            }
                        }
                        1 => {
                            self.send_size = val as usize;
                            State::SendDaArgs {
                                idx: 2,
                                buf: [0; 4],
                                n: 0,
                            }
                        }
                        _ => {
                            self.send_siglen = val;
                            // Reject if SLA has not been bypassed
                            let status = if self.profile.sla_required && !self.bypassed {
                                wire::BROM_STATUS_SLA_REQUIRED
                            } else {
                                0
                            };
                            self.push(&status.to_be_bytes());
                            self.ev(format!(
                                "SEND_DA: addr=0x{:08X} size={} siglen={} status=0x{:04X}",
                                self.send_addr, self.send_size, self.send_siglen, status
                            ));
                            if status != 0 {
                                State::Brom
                            } else {
                                self.da_buf.clear();
                                State::SendDaData
                            }
                        }
                    }
                }
            }
            State::SendDaData => {
                self.da_buf.push(b);
                if self.da_buf.len() >= self.send_size {
                    let sum: u32 = self.da_buf.iter().map(|x| *x as u32).sum::<u32>() & 0xFFFF;
                    self.push(&(sum as u16).to_be_bytes());
                    self.push(&0u16.to_be_bytes());
                    self.ev(format!(
                        "SEND_DA: 收到 {} 字节, checksum=0x{:04X}",
                        self.da_buf.len(),
                        sum
                    ));
                    self.da_stage = self.da_stage.saturating_add(1);
                    State::Brom
                } else {
                    State::SendDaData
                }
            }
            State::JumpEcho { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::JumpEcho { buf, n: n + 1 }
                } else {
                    self.push(&buf);
                    State::JumpTrigger { buf: [0; 4], n: 0 }
                }
            }
            State::JumpTrigger { mut buf, n } => {
                buf[n] = b;
                if n + 1 < 4 {
                    State::JumpTrigger { buf, n: n + 1 }
                } else {
                    let addr = u32::from_be_bytes(buf);
                    self.ev(format!("JUMP_DA: 0x{:08X}", addr));
                    self.push(&[wire::BROM_JUMP_ACK]);
                    // DA started → send SYNC
                    self.push(&wire::XFLASH_SYNC_SIGNAL.to_le_bytes());
                    self.ev("DA: SYNC 已发送");
                    State::DaHeader { buf: [0; 12], n: 0 }
                }
            }
            State::DaHeader { mut buf, n } => {
                buf[n] = b;
                if n + 1 < wire::XFLASH_HEADER_LEN {
                    State::DaHeader { buf, n: n + 1 }
                } else {
                    let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
                    let dtype = u32::from_le_bytes(buf[4..8].try_into().unwrap());
                    let len = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
                    if magic != wire::XFLASH_MAGIC {
                        self.ev(format!("DA: 包头 magic 错误 0x{:08X}", magic));
                        // Resync: discard until the next magic (simplified: return to header state)
                        return Ok(());
                    }
                    let _ = dtype;
                    if len > 1024 * 1024 {
                        self.ev(format!("DA: 包长异常 {}", len));
                        return Ok(());
                    }
                    State::DaPayload {
                        len,
                        buf: Vec::with_capacity(len),
                    }
                }
            }
            State::ExploitId => {
                let id = b as usize;
                let accepted = id < SIM_EXPLOITS.len()
                    && self.profile.exploits.iter().any(|n| n == SIM_EXPLOITS[id]);
                let status = if accepted {
                    self.bypassed = true;
                    self.ev(format!("SIM exploit 接受: {}", SIM_EXPLOITS[id]));
                    0u8
                } else {
                    self.ev(format!("SIM exploit 拒绝: id={}", b));
                    1u8
                };
                self.push(&[wire::SIM_EXPLOIT_CMD, b, status]);
                State::Brom
            }
            State::DaPayload { len, mut buf } => {
                buf.push(b);
                if buf.len() >= len {
                    self.on_da_payload(&buf)?;
                    State::DaHeader { buf: [0; 12], n: 0 }
                } else {
                    State::DaPayload { len, buf }
                }
            }
        };
        Ok(())
    }

    fn on_brom_cmd(&mut self, b: u8) -> Result<State, PortError> {
        // Single-byte command: echo first, then respond
        self.push(&[b]);
        self.on_brom_cmd_after_echo(b)
    }

    fn on_brom_cmd_after_echo(&mut self, cmd: u8) -> Result<State, PortError> {
        Ok(match cmd {
            wire::BROM_GET_HW_CODE => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.hw_code.to_be_bytes());
                resp.extend_from_slice(&self.profile.hw_sub.to_be_bytes());
                self.push(&resp);
                self.ev(format!(
                    "GET_HW_CODE: 0x{:04X}/0x{:04X}",
                    self.profile.hw_code, self.profile.hw_sub
                ));
                State::Brom
            }
            wire::BROM_GET_TARGET_CONFIG => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.safety.to_be_bytes());
                resp.extend_from_slice(&0u16.to_be_bytes());
                self.push(&resp);
                self.ev(format!("GET_TARGET_CONFIG: 0x{:08X}", self.profile.safety));
                State::Brom
            }
            wire::BROM_SYNC => {
                let mut resp = Vec::new();
                resp.extend_from_slice(&self.profile.hw_ver.to_be_bytes());
                resp.extend_from_slice(&self.profile.sw_ver.to_be_bytes());
                resp.extend_from_slice(&0u16.to_be_bytes());
                resp.extend_from_slice(&[0x01, 0x00]);
                self.push(&resp);
                self.ev("SYNC(0xFC): ok");
                State::Brom
            }
            0xFE => {
                self.push(&[wire::BROM_SYNC_FE_ACK]);
                State::Brom
            }
            0xFF => State::Brom,
            wire::BROM_SEND_DA => State::SendDaArgs {
                idx: 0,
                buf: [0; 4],
                n: 0,
            },
            wire::BROM_JUMP_DA => State::JumpEcho { buf: [0; 4], n: 0 },
            // SIM: exploit handshake `0x1F <id>`, id agreed between host and device
            0xD6 => {
                self.push(&[0x00, 0x00]);
                self.push(&[0x00, 0x00]);
                State::Brom
            }
            other => {
                self.ev(format!("BROM: 未支持命令 0x{:02X}", other));
                State::Brom
            }
        })
    }

    fn on_da_payload(&mut self, payload: &[u8]) -> Result<(), PortError> {
        // ── Write transaction: three parameter packets [0u32]`checksum``data` form one block, with one status at block end
        if self.write.is_some() {
            let done = self.on_write_param(payload)?;
            if done {
                return Ok(());
            }
        }
        // ── FORMAT two-step ack
        if self.format_stage > 0 && payload.len() == 4 {
            self.on_format_ack(payload)?;
            return Ok(());
        }
        // Parameter block (length 56, or immediately following a command that expects parameters)
        if let Some(cmd) = self.pending_cmd {
            if payload.len() != 4 || !is_command_word(payload) {
                self.on_da_param(cmd, payload)?;
                self.pending_cmd = None;
                return Ok(());
            }
            // Length 4 and a known command → still handle as a command (drop the pending parameter to avoid getting stuck)
            self.pending_cmd = None;
        }
        if payload.len() != 4 {
            self.ev(format!("DA: 未知负载 len={}", payload.len()));
            self.push_status(0);
            return Ok(());
        }
        let cmd = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        self.ev(format!("DA cmd 0x{:06X}", cmd));
        match cmd {
            wire::XFLASH_CMD_READ_DATA => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_DEVICE_CTRL => {
                self.push_status(0);
            }
            wire::XFLASH_CMD_EMMC_INFO => {
                if self.profile.emmc_info {
                    self.push_status(0);
                    let info = self.emmc_info_payload();
                    self.push_packet(wire::XFLASH_TYPE_FLOW, &info);
                    self.push_status(0);
                } else {
                    self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
                }
            }
            wire::XFLASH_CMD_GET_DA_VER => {
                self.push_status(0);
                self.push_packet(wire::XFLASH_TYPE_FLOW, &0x0000_0107u32.to_le_bytes());
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_ACK => {
                self.push_status(0);
                self.push_packet(
                    wire::XFLASH_TYPE_FLOW,
                    &wire::XFLASH_ACK_MAGIC.to_le_bytes(),
                );
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_SET_STORAGE => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_WRITE_DATA => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_CMD_FORMAT => {
                self.pending_cmd = Some(cmd);
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT => {
                self.push_status(0);
            }
            other => {
                self.ev(format!("DA: 不支持命令 0x{:06X}", other));
                self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
            }
        }
        Ok(())
    }

    fn on_da_param(&mut self, cmd: u32, param: &[u8]) -> Result<(), PortError> {
        match cmd {
            wire::XFLASH_CMD_READ_DATA => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let parttype = u32::from_le_bytes(param[4..8].try_into().unwrap());
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.pending_read = Some((addr, size));
                self.ev(format!(
                    "READ_DATA: addr=0x{:X} size={} parttype={}",
                    addr, size, parttype
                ));
                self.push_status(0);
                let data = self.read_window(addr, size);
                self.push_packet(wire::XFLASH_TYPE_FLOW, &data);
                self.push_status(0);
            }
            wire::XFLASH_DEVCTRL_SET_STORAGE => {
                let t = if param.len() >= 4 {
                    u32::from_le_bytes(param[0..4].try_into().unwrap())
                } else {
                    0
                };
                self.ev(format!("SET_STORAGE: type={}", t));
                self.push_status(0);
            }
            wire::XFLASH_CMD_WRITE_DATA => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let parttype = u32::from_le_bytes(param[4..8].try_into().unwrap());
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.ev(format!(
                    "WRITE_DATA: addr=0x{:X} size={} parttype={}",
                    addr, size, parttype
                ));
                self.write = Some(WriteSession {
                    addr,
                    size,
                    written: 0,
                    stage: 0,
                    checksum: 0,
                });
                self.push_status(0);
            }
            wire::XFLASH_CMD_FORMAT => {
                if param.len() < 24 {
                    self.push_status(1);
                    return Ok(());
                }
                let addr = u64::from_le_bytes(param[8..16].try_into().unwrap());
                let size = u64::from_le_bytes(param[16..24].try_into().unwrap());
                self.erase_range(addr, size);
                self.ev(format!("FORMAT: addr=0x{:X} size={}", addr, size));
                self.push_status(0);
                // Device-side time consumption: CONTINUE + wait milliseconds, then send COMPLETE after host ack
                self.push_status(wire::XFLASH_STATUS_CONTINUE);
                self.push_status(2);
                self.format_stage = 1;
            }
            other => {
                self.ev(format!("DA: 命令 0x{:06X} 不接受参数", other));
                self.push_status(wire::XFLASH_STATUS_UNSUPPORTED);
            }
        }
        Ok(())
    }
}

fn is_command_word(payload: &[u8]) -> bool {
    if payload.len() != 4 {
        return false;
    }
    let cmd = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    matches!(
        cmd,
        wire::XFLASH_CMD_READ_DATA
            | wire::XFLASH_CMD_DEVICE_CTRL
            | wire::XFLASH_CMD_EMMC_INFO
            | wire::XFLASH_CMD_GET_DA_VER
            | wire::XFLASH_DEVCTRL_ACK
            | wire::XFLASH_DEVCTRL_SET_STORAGE
            | wire::XFLASH_CMD_WRITE_DATA
            | wire::XFLASH_CMD_FORMAT
            | wire::XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT
    )
}
