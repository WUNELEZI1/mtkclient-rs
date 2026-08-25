//! 写包长度协商（SET_PKT_LEN）与写命令发送

use log::{trace, warn};

use crate::da::xflash::{CMD_MAGIC, CMD_WRITE_DATA, DAXFlash, pack3};
use crate::da::xflash::protocol::{GET_PKT_LEN, SET_PKT_LEN};

impl<'a> DAXFlash<'a> {
    /// 获取写包长度（对齐 Python get_packet_length），并主动抬升工作传输单元。
    ///
    /// 性能关键：刷机匣（GeekFlashTool）实测在 preloader/DA 模式下把包长度设到
    /// 2MB（0x200000）批量传输；而旧实现仅在 `GET_PKT_LEN` 失败时回退到 256KB
    /// （0x40000），导致写入被切成 8 倍多的 USB 往返，速度远低于刷机匣。
    /// 这里统一把工作单元抬到 2MB：DA 不支持 `SET_PKT_LEN` 时 `send_devctrl` 回空/
    /// 报错，此处忽略，不影响既有行为（无回归风险）。
    pub(crate) fn get_packet_length(&mut self) -> Result<usize, String> {
        // GET_PACKET_LENGTH (0x040007) — DA 回报当前写包长度；传输级错误照常上抛。
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
            if plen > 0 {
                Some(plen as usize)
            } else {
                None
            }
        } else if data.len() >= 4 {
            let plen = u32::from_le_bytes(data[..4].try_into().unwrap());
            if plen > 0 {
                Some(plen as usize)
            } else {
                None
            }
        } else {
            None
        };
        // 工作传输单元：已 ≥2MB 则沿用（不降速），否则抬到 2MB（0x200000），上限 4MB（0x400000）。
        // 性能关键：对齐刷机匣批量传输；DA 不支持 SET_PKT_LEN 时 send_devctrl 回空/报错，忽略即可。
        let mut target = match reported {
            Some(v) if v >= 0x200000 => v.min(0x400000),
            _ => 0x200000,
        };
        // 主动 SET_PKT_LEN 抬升。SET 成功后重新 GET 确认 DA 实际接受的包长度，
        // 防 DA 端溢出：部分 DA 只接受更小的值，盲目按请求值传输会触发溢出/截断。
        // 仅当 DA 明确回报更小正数时才回退，GET 失败/返回 0 沿用请求值。
        if self.send_devctrl(SET_PKT_LEN, Some(&(target as u32).to_le_bytes())).is_ok() {
            trace!(
                "DA 写包长度已提升至 {} 字节 ({:.2} MiB)",
                target,
                target as f64 / 1024.0 / 1024.0
            );
            if let Ok(ack) = self.send_devctrl(GET_PKT_LEN, None) {
                let actual = if ack.len() >= 4 {
                    u32::from_le_bytes(ack[..4].try_into().unwrap())
                } else {
                    0
                };
                if actual > 0 && (actual as usize) < target {
                    warn!(
                        "DA 实际接受写包长度 {} < 请求 {}，回退以避免 DA 端溢出",
                        actual, target
                    );
                    target = actual as usize;
                }
            }
        } else {
            trace!("SET_PKT_LEN 不受支持（已忽略）");
        }
        Ok(target)
    }

    /// 发送写命令（对齐 Python cmd_write_data）
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

        // 对齐 Python：发送 WRITE_DATA，失败时仅 drain + 重试（不发 xflash_sync）
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
                param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
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
