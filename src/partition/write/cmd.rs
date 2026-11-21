//! Write packet length negotiation (SET_PKT_LEN) and write command dispatch

use log::{trace, warn};

use crate::da::xflash::protocol::{GET_PKT_LEN, SET_PKT_LEN};
use crate::da::xflash::{CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};

impl<'a> DAXFlash<'a> {
    /// Gets the write packet length (mirrors Python get_packet_length) and actively
    /// raises the working transfer unit.
    ///
    /// Performance-critical: GeekFlashTool was measured to use a 2MB (0x200000) packet
    /// length in preloader/DA mode for bulk transfers, whereas the old implementation
    /// fell back to 256KB (0x40000) only when `GET_PKT_LEN` failed, splitting the write
    /// into 8x more USB round trips and running far slower than GeekFlashTool. Here the
    /// working unit is raised to 2MB uniformly: when the DA does not support
    /// `SET_PKT_LEN`, `send_devctrl` returns empty/errors, which is ignored here and
    /// does not change existing behavior (no regression risk).
    pub(crate) fn get_packet_length(&mut self) -> Result<usize, String> {
        // GET_PACKET_LENGTH (0x040007) - the DA reports the current write packet length; transport errors propagate as usual.
        let data = self.send_devctrl(GET_PKT_LEN, None)?;
        let reported = if data.len() >= 8 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            let read_plen = u32::from_le_bytes(data[4..8].try_into().unwrap());
            trace!(
                "DA 写包长度: {} 字节 ({:.2} MiB), 读包长度: {} 字节 ({:.2} MiB)",
                plen,
                plen as f64 / 1024.0 / 1024.0,
                read_plen,
                read_plen as f64 / 1024.0 / 1024.0
            );
            if plen > 0 { Some(plen as usize) } else { None }
        } else if data.len() >= 4 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            if plen > 0 { Some(plen as usize) } else { None }
        } else {
            None
        };
        // Working transfer unit: **trust the value reported by the DA**.
        //
        // Mirrors mtkclient `get_packet_length` (xflash_lib.py): GET only, no SET, then
        // `write_packet_size = plen.write_packet_length`.
        // An earlier implementation treated 0x040008 as SET_PACKET_LENGTH (it is
        // actually GET_RANDOM_ID) and forced the packet length up to 2 MiB; on a real DA
        // that is "sending a GET with an argument", which may desync the state machine
        // and truncate data when it exceeds the DA buffer. So it was changed to:
        //   1. use the DA-reported value; fall back to 256 KiB (the old lower bound) when
        //      it reports 0 or fails;
        //   2. raise it actively only when explicitly enabled via the environment
        //      variable MTK_FORCE_PKT_LEN=<bytes>.
        let mut target = match reported {
            Some(v) if v > 0 => v,
            _ => 0x4_0000,
        };
        if let Ok(force) = std::env::var("MTK_FORCE_PKT_LEN")
            && let Ok(v) = force.trim().parse::<usize>()
            && v >= 0x200
        {
            target = v.min(0x40_0000);
            if self
                .send_devctrl(SET_PKT_LEN, Some(&(target as u32).to_le_bytes()))
                .is_ok()
            {
                warn!(
                    "已按 MTK_FORCE_PKT_LEN 请求包长 {} 字节（DA 支持性未经验证）",
                    target
                );
            }
        }
        Ok(target)
    }

    /// Sends the write command (mirrors Python cmd_write_data).
    pub(crate) fn cmd_write_data(
        &mut self,
        addr: u64,
        size: u64,
        storage: u32,
        parttype: u32,
    ) -> Result<bool, String> {
        trace!(
            "[cmd_write_data] addr=0x{:08X} size={} storage={} parttype={}",
            addr, size, storage, parttype
        );

        // Mirrors Python: send WRITE_DATA, on failure only drain + retry (no xflash_sync)
        for attempt in 0..3 {
            if attempt > 0 {
                warn!(
                    "[cmd_write_data] 第 {} 次尝试失败，执行 drain + 重试...",
                    attempt
                );
                self.preloader.device.drain_pending();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }

            // xsend(WRITE_DATA)
            let pkt = pack3(CMD_MAGIC, 0x01, 4);
            if let Err(e) = self.write_with_retry(&pkt, "cmd_write_data xsend") {
                trace!("[cmd_write_data] write header 失败: {}", e);
                continue;
            }
            if let Err(e) =
                self.write_with_retry(&CMD_WRITE_DATA.to_le_bytes(), "cmd_write_data CMD")
            {
                trace!("[cmd_write_data] write CMD 失败: {}", e);
                continue;
            }

            let mut st = match self.status() {
                Ok(s) => s,
                Err(e) => {
                    trace!("[cmd_write_data] status 读取失败: {}", e);
                    continue;
                }
            };
            if st == CMD_WRITE_DATA {
                warn!("cmd_write_data 读到 WRITE_DATA echo，尝试再读一次 status 进行重同步");
                st = match self.status() {
                    Ok(s) => s,
                    Err(e) => {
                        trace!("[cmd_write_data] 二次 status 读取失败: {}", e);
                        continue;
                    }
                };
            }
            if st == 0 {
                trace!("[cmd_write_data] status ok, 发送 56B 参数");
                let mut param = Vec::with_capacity(56);
                param.extend_from_slice(&storage.to_le_bytes());
                param.extend_from_slice(&parttype.to_le_bytes());
                param.extend_from_slice(&addr.to_le_bytes());
                param.extend_from_slice(&size.to_le_bytes());
                param.extend_from_slice(&[0u8; 32]); // NandExtension, all zeros
                self.send_param_list_chunked(&[&param], "cmd_write_data param")?;
                return Ok(true);
            }
            warn!(
                "[cmd_write_data] 尝试 {}/2 失败: status=0x{:08X}",
                attempt + 1,
                st
            );
        }

        Err(format!(
            "cmd_write_data 多次尝试后仍然失败。可能原因：DA 状态机未同步，或设备端写入超时。\
             建议：1) 让设备重新进入 BROM 模式后重试；2) 检查 USB 连接稳定性。"
        ))
    }
}
