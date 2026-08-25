//! 协议原语 — 块 3: 同步 / devctrl

use super::{CMD_MAGIC, CMD_SYNC_SIGNAL, SHORT_QUERY_TIMEOUT_MS, SLA_QUERY_TIMEOUT_MS, pack3};
use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// XFlash 同步命令
    /// Python: sync() → 只 xsend(CMD_SYNC_SIGNAL)，不读 status 也不读 response
    ///
    /// 警告：此方法只发送 SYNC_SIGNAL，不读取任何响应。
    /// 在 DA 加载流程（upload_da1）中调用时，DA 可能不返回 status，
    /// 如果强制读取会导致超时失败。调用者如需清除残留数据，
    /// 应在适当时候手动调用 drain_pending()。
    pub(crate) fn xflash_sync(&mut self) -> Result<bool, String> {
        trace!("执行 XFlash 同步命令...");

        // Python: self.sync() → self.xsend(self.Cmd.SYNC_SIGNAL) = 0x434E5953
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SYNC_SIGNAL.to_le_bytes())?;

        trace!("[SYNC] 已发送 SYNC_SIGNAL (0x434E5953)");
        Ok(true)
    }

    /// 发送 devctrl 命令
    /// Python: 任何阶段都返回 b""，不抛异常
    ///
    /// 关键修复：即使 status 非零（如 0x00010009）也必须完成完整的 3 步握手
    ///（DEVICE_CTRL → cmd → param/status），否则 DA 状态机卡在等待阶段，
    /// 后续任何命令都会超时。
    pub(crate) fn send_devctrl(
        &mut self,
        cmd: u32,
        param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        trace!(
            "[send_devctrl] cmd=0x{:06X} param={}",
            cmd,
            param.map_or(0, |p| p.len())
        );

        // xsend(Cmd.DEVICE_CTRL) — DEVICE_CTRL = 0x010009
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;

        let st = self.status()?;
        let stage1_ok = st == 0;
        if !stage1_ok {
            // SYNC (0x434E5953) = DA 初始化信号残留在 USB 缓冲区
            // 对齐 Python send_devctrl：stage1 != 0 时不发送 cmd，直接返回空
            // 这样避免 cmd 响应被排入队列导致后续命令读到过期数据
            if st == CMD_SYNC_SIGNAL {
                trace!(
                    "[send_devctrl] stage1=SYNC (DA init residual), 跳过 cmd 0x{:06X}",
                    cmd
                );
                // 只读取并丢弃一个残留包（不用 drain 循环，避免消耗后续命令的响应）
                let _ = self.status();
                return Ok(vec![]);
            }
            // 已知正常状态码：静默处理
            // 0x00010009 = DEVICE_CTRL 不支持（部分设备/DA版本）
            // 0xC0010004 = 命令不支持
            if st != 0xC0010004 && st != 0x00010009 {
                warn!("send_devctrl DEVICE_CTRL 阶段1 状态: 0x{:08X}", st);
            }
            trace!("[send_devctrl] stage1 status=0x{:08X}, 继续完成握手...", st);
        }

        // xsend(cmd) — stage1 成功时发送（SYNC 已在上方提前返回）
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;

        let st2 = self.status()?;
        let stage2_ok = st2 == 0;
        if !stage2_ok {
            if st2 != 0xC0010004 && st2 != 0x00010009 {
                warn!("send_devctrl(0x{:06X}) 阶段2 状态: 0x{:08X}", cmd, st2);
            }
            trace!(
                "[send_devctrl] stage2 status=0x{:08X}, 继续完成握手...",
                st2
            );
        }

        // 只有 param 模式需要两个阶段都成功才继续
        // read 模式：stage1/stage2 都成功时才尝试 xread
        // 如果 stage2 返回 0xC0010004（命令不支持）或 0x00010009，DA 不会发送 xread 数据
        if param.is_none() {
            if stage2_ok {
                let resp = self.xread_data()?;
                let _ = self.status();
                trace!(
                    "[send_devctrl] cmd=0x{:06X} xread returned {} bytes",
                    cmd,
                    resp.len()
                );
                return Ok(resp);
            } else {
                // 命令不支持或设备不支持，返回空数据（对齐 Python: 任何阶段都返回 b""）
                trace!(
                    "[send_devctrl] cmd=0x{:06X} stage2 不支持(0x{:08X})，返回空",
                    cmd, st2
                );
                return Ok(vec![]);
            }
        }

        // param 模式：两个阶段都成功才继续发送 param
        if stage1_ok && stage2_ok {
            if let Some(p) = param {
                let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
                self.preloader.device.write(&pkt3)?;
                self.preloader.device.write(p)?;
                let st3 = self.status()?;
                if st3 != 0 {
                    // param status 非零表示命令执行异常，返回错误以便调用者处理
                    return Err(format!(
                        "send_devctrl(0x{:06X}) param status: 0x{:08X}",
                        cmd, st3
                    ));
                }
            }
        } else {
            // read 模式已在上方处理；param 模式 stage 失败时仍需发送 param 完成握手
            if let Some(p) = param {
                let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
                let _ = self.preloader.device.write(&pkt3);
                let _ = self.preloader.device.write(p);
                let _ = self.status(); // 读取并丢弃 status3，完成握手
            }
        }

        Ok(vec![])
    }

    /// 临时设置短超时执行回调，完成后恢复原超时
    /// 用于可选查询：命令不支持时快速失败，不等 5 秒
    pub(crate) fn with_short_timeout<T>(
        &mut self,
        ms: u64,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let orig = self.preloader.device.get_timeout();
        self.preloader.device.set_timeout(Duration::from_millis(ms));
        let result = f(self);
        self.preloader.device.set_timeout(orig);
        result
    }

    /// 获取连接代理（brom 或 preloader）
    /// Python: 返回 b"" 或 None 时视为失败
    pub(crate) fn get_connection_agent(&mut self) -> Result<String, String> {
        let data =
            self.with_short_timeout(SHORT_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x010102, None))?;
        if data.is_empty() {
            return Err("get_connection_agent returned empty".to_string());
        }
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// DA 扩展命令：CUSTOM_READREGISTER (0x0F0002)
    /// 对齐 Python xflash.py: custom_readregister(addr)
    /// 流程: cmd(0x0F0002) → xsend(addr) → xread() → status()
    pub(crate) fn custom_readregister(&mut self, addr: u32) -> Result<u32, String> {
        // step 1: cmd(0x0F0002) — send DEVICE_CTRL + CUSTOM_READREGISTER
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!(
                "custom_readregister: DEVICE_CTRL status=0x{:08X}",
                st1
            ));
        }

        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0F0002u32.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "custom_readregister: CUSTOM_READREGISTER status=0x{:08X}",
                st2
            ));
        }

        // step 2: xsend(addr)
        let pkt3 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt3)?;
        self.preloader.device.write(&addr.to_le_bytes())?;

        // step 3: xread() — read 4-byte register value
        let data = self.xread_data()?;

        // step 4: status()
        let st3 = self.status()?;
        if st3 != 0 {
            return Err(format!("custom_readregister: read status=0x{:08X}", st3));
        }

        if data.len() < 4 {
            return Err(format!(
                "custom_readregister: response too short: {} bytes",
                data.len()
            ));
        }
        Ok(u32::from_le_bytes(data[0..4].try_into().unwrap()))
    }

    /// DA 扩展命令：CUSTOM_WRITEREGISTER (0x0F0004)
    /// 对齐 Python xflash.py: custom_writeregister(addr, data)
    /// 流程: cmd(0x0F0004) → xsend(addr) → xsend(data) → status()
    pub(crate) fn custom_writeregister(&mut self, addr: u32, value: u32) -> Result<(), String> {
        // step 1: cmd(0x0F0004) — send DEVICE_CTRL + CUSTOM_WRITEREGISTER
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;
        let st1 = self.status()?;
        if st1 != 0 {
            return Err(format!(
                "custom_writeregister: DEVICE_CTRL status=0x{:08X}",
                st1
            ));
        }

        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0F0004u32.to_le_bytes())?;
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "custom_writeregister: CUSTOM_WRITEREGISTER status=0x{:08X}",
                st2
            ));
        }

        // step 2: xsend(addr)
        let pkt3 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt3)?;
        self.preloader.device.write(&addr.to_le_bytes())?;

        // step 3: xsend(value)
        let pkt4 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt4)?;
        self.preloader.device.write(&value.to_le_bytes())?;

        // step 4: status()
        let st3 = self.status()?;
        if st3 != 0 {
            return Err(format!("custom_writeregister: write status=0x{:08X}", st3));
        }

        Ok(())
    }

    /// 设置重置键
    pub(crate) fn set_reset_key(&mut self, key: u32) -> Result<(), String> {
        self.send_devctrl(0x010103, Some(&key.to_le_bytes()))?;
        Ok(())
    }

    /// 设置校验级别
    pub(crate) fn set_checksum_level(&mut self, level: u32) -> Result<(), String> {
        self.send_devctrl(0x010104, Some(&level.to_le_bytes()))?;
        Ok(())
    }

    /// 获取过期日期（短超时：200ms，不支持时快速跳过）
    pub(crate) fn get_expire_date(&mut self) -> Result<Vec<u8>, String> {
        let data =
            self.with_short_timeout(SHORT_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x010105, None))?;
        if data.is_empty() {
            return Err("get_expire_date returned empty".to_string());
        }
        Ok(data)
    }

    /// 获取 SLA 状态（短超时：200ms，不支持时快速跳过）
    pub(crate) fn get_sla_status(&mut self) -> Result<u32, String> {
        let data =
            self.with_short_timeout(SLA_QUERY_TIMEOUT_MS, |da| da.send_devctrl(0x01010E, None))?; // SLA_ENABLED_STATUS
        if data.len() >= 4 {
            Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
        } else {
            Err("sla_status empty".to_string())
        }
    }

    /// 设置 Meta Boot Mode（对齐 Python set_meta_boot_mode）
    /// 命令: CMD_DEVICE_CTRL(0x010009) + SET_META_BOOT_MODE(0x020006)
    /// 参数: boot_mode (u32, 0=fastboot, 1=meta)
    pub(crate) fn set_meta_boot_mode(&mut self, boot_mode: u32) -> Result<(), String> {
        self.send_devctrl(0x020006, Some(&boot_mode.to_le_bytes()))?;
        Ok(())
    }

    /// 查询当前 USB 速度（对齐 Python get_usb_speed）
    /// 返回 "full-speed" / "high-speed" / "super-speed" 或 "unknown"
    pub(crate) fn get_usb_speed(&mut self) -> Result<String, String> {
        let data = self.send_devctrl(0x010115, None)?; // GET_USB_SPEED
        if data.is_empty() {
            return Err("get_usb_speed 返回空".to_string());
        }
        // 设备可能返回数值 (u32 LE) 或字符串，需要兼容两种格式
        let speed = if data.len() >= 4 {
            let val = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            match val {
                0 => "full-speed",
                1 => "high-speed",
                2 => "super-speed",
                _ => {
                    // 不是已知数值，尝试作为 UTF-8 字符串解析
                    let s = String::from_utf8_lossy(&data).trim().to_string();
                    if s.is_empty() || !s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                        trace!("  USB 速度: 未知值 0x{:08X}, raw={:02X?}", val, &data);
                        return Ok("unknown".to_string());
                    }
                    // 返回有效的字符串（使用 intern 避免分配）
                    return Ok(s);
                }
            }
            .to_string()
        } else {
            // 数据不足 4 字节，尝试字符串解析
            let s = String::from_utf8_lossy(&data).trim().to_string();
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                trace!("  USB 速度: raw={:02X?}", &data);
                "unknown".to_string()
            } else {
                s
            }
        };
        trace!("  USB 速度: {}", speed);
        Ok(speed)
    }

    /// 命令设备切换到更高 USB 速度（对齐 Python set_usb_speed）
    /// 发送 SWITCH_USB_SPEED + magic 0x0E8D2001
    pub(crate) fn set_usb_speed(&mut self) -> Result<(), String> {
        // Python: self.xsend(self.Cmd.SWITCH_USB_SPEED) + status
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010114u32.to_le_bytes())?; // SWITCH_USB_SPEED
        let st = self.status()?;
        if st != 0 {
            return Err(format!("SWITCH_USB_SPEED 状态: 0x{:08X}", st));
        }

        // Python: self.xsend(pack("<I", 0x0E8D2001)) + status
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&0x0E8D2001u32.to_le_bytes())?; // MediaTek USB VID magic
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("set_usb_speed magic 状态: 0x{:08X}", st2));
        }

        info!("已发送 USB 速度切换命令");
        Ok(())
    }
}
