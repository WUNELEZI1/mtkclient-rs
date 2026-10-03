//! 协议原语 — 块 4: SHUTDOWN / XML DA 命令 (Layer 2 + Layer 3)

use super::{CMD_MAGIC, CMD_SHUTDOWN, SHUTDOWN_BOOTMODE_HOME_SCREEN, hex_str, pack3};
use log::{info, trace};

use crate::da::xml::XmlSession;
use crate::error::ProtocolError;

use crate::da::xflash::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// Layer 2: XFlash DA SHUTDOWN 命令（对齐 xflash_lib.py shutdown）
    ///
    /// 通过 XFlash 二进制协议发送 SHUTDOWN 命令，指定 bootmode 控制重启行为。
    ///
    /// 命令格式：
    ///   头: MAGIC(4) + SHUTDOWN_CMD(4) = 8 字节
    ///   参数体: 32 字节（hasflags + enablewdt + async_mode + bootmode + 保留）
    ///   响应: status(4)
    ///
    /// bootmode 说明：
    ///   1 = HOME_SCREEN（重启到系统）— 本工程统一使用该模式
    pub fn da_shutdown(&mut self) -> Result<(), String> {
        let bootmode = SHUTDOWN_BOOTMODE_HOME_SCREEN;
        info!("[DA SHUTDOWN] bootmode={} (重启到系统)", bootmode);

        // 发送命令头: MAGIC + CMD_SHUTDOWN
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader
            .device
            .write(&hdr)
            .map_err(|e| format!("SHUTDOWN write hdr: {}", e))?;
        self.preloader
            .device
            .write(&CMD_SHUTDOWN.to_le_bytes())
            .map_err(|e| format!("SHUTDOWN write cmd: {}", e))?;

        // 读取阶段 1 status
        let st = self.status()?;
        if st != 0 {
            return Err(format!(
                "SHUTDOWN 命令 {}",
                ProtocolError::StatusNonZero(st)
            ));
        }

        // 构建 32 字节参数体 — 严格对齐 mtkclient xflash_lib.py shutdown：
        //   pack("<IIIIIIII", hasflags, enablewdt, async_mode, bootmode,
        //        dl_bit, dont_resetrtc, leaveusb, 0)
        // 关键修正（对比旧实现，已对照 mtkclient 权威源码）：
        //   - enablewdt = 0（禁用看门狗）。MTK DA 的 reboot-to-system 由 bootmode 控制：
        //     DA 收到 SHUTDOWN 后直接跳转到下一启动阶段（preloader → system），
        //     无需看门狗硬复位。mtkclient 源码固定 enablewdt=0 且 reboot 正常；
        //     旧实现误用 enablewdt=1，看门狗触发硬件复位后设备（USB 仍连接）会重新
        //     掉回 Preloader/下载模式而非进入系统，表现为"日志显示重启成功但设备未进系统"。
        //   - 参数体 32 字节(8×u32)，字段顺序严格对齐 mtkclient（含末尾保留字段）。
        // 字段顺序(均为 u32 LE)：
        //   0x00 hasflags      非 NORMAL 模式 / async / dl_bit 时为 1，否则 0
        //   0x04 enablewdt     0 = 禁用 WDT（DA 跳转重启，不靠看门狗）
        //   0x08 async_mode    0
        //   0x0C bootmode      1 = HOME_SCREEN（重启到系统）
        //   0x10 dl_bit        0
        //   0x14 dont_resetrtc 0
        //   0x18 leaveusb      0（重启后断开 USB，便于设备重新枚举）
        //   0x1C 保留          0
        let async_mode: u32 = 0;
        let dl_bit: u32 = 0;
        let hasflags: u32 = if bootmode != 0 || async_mode != 0 || dl_bit != 0 {
            1
        } else {
            0
        };
        let enablewdt: u32 = 0; // 禁用看门狗：DA 收到 SHUTDOWN 后直接跳转重启到系统（对齐 mtkclient shutdown 固定 enablewdt=0）
        let mut param = [0u8; 32];
        param[0x00..0x04].copy_from_slice(&hasflags.to_le_bytes());
        param[0x04..0x08].copy_from_slice(&enablewdt.to_le_bytes());
        param[0x08..0x0C].copy_from_slice(&async_mode.to_le_bytes());
        param[0x0C..0x10].copy_from_slice(&bootmode.to_le_bytes());
        param[0x10..0x14].copy_from_slice(&dl_bit.to_le_bytes());
        param[0x14..0x18].copy_from_slice(&0u32.to_le_bytes()); // dont_resetrtc
        param[0x18..0x1C].copy_from_slice(&0u32.to_le_bytes()); // leaveusb
        // 0x1C..0x20 保留 0

        trace!("[DA SHUTDOWN] param (32B): {}", hex_str(&param));

        // 发送参数体（32 字节）
        let param_pkt = pack3(CMD_MAGIC, 0x01, 32);
        self.preloader
            .device
            .write(&param_pkt)
            .map_err(|e| format!("SHUTDOWN write param hdr: {}", e))?;
        self.preloader
            .device
            .write(&param)
            .map_err(|e| format!("SHUTDOWN write param: {}", e))?;

        // 读取阶段 2 status
        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!(
                "SHUTDOWN 参数 {}",
                ProtocolError::StatusNonZero(st2)
            ));
        }

        info!(
            "[DA SHUTDOWN] 成功 (bootmode={}, enablewdt=0x{:02X})",
            bootmode, enablewdt
        );
        Ok(())
    }

    /// Layer 3: 通过 XML DA 协议重启到指定模式（新平台 MT6789+）
    ///
    /// 流程：先发送 `SET-BOOT-MODE(<mode>)`，再发送 `REBOOT(IMMEDIATE)`。
    /// 会话原语（CMD:START/END 生命周期、帧编解码、ACK）统一由
    /// [`crate::da::xml::XmlSession`] 处理，与 `xml` 子命令走同一实现。
    fn da_xml_reboot_mode(&mut self, mode: &str) -> Result<(), String> {
        info!("[XML DA] SET-BOOT-MODE: {}", mode);
        let mut session = XmlSession::new(&mut *self.preloader.device);
        session
            .set_boot_mode(mode, "USB", "ON", "ON")
            .map_err(|e| format!("XML SET-BOOT-MODE 失败: {}", e))?;

        info!("[XML DA] REBOOT: IMMEDIATE");
        session
            .reboot(false)
            .map_err(|e| format!("XML REBOOT 失败: {}", e))?;

        info!("[XML DA] 设备将重启到 {}", mode);
        Ok(())
    }

    /// Layer 3: 通过 XML DA 协议重启到 fastboot（新平台 MT6789+）
    pub fn da_xml_reboot_fastboot(&mut self) -> Result<(), String> {
        self.da_xml_reboot_mode("FASTBOOT")
    }

    /// Layer 3: 通过 XML DA 协议重启到 meta（新平台）
    pub fn da_xml_reboot_meta(&mut self) -> Result<(), String> {
        self.da_xml_reboot_mode("META")
    }
}
