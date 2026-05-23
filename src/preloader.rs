use crate::config::{ChipConfig, TargetConfig, CHIP_CONFIGS};
use crate::usb::UsbDevice;
use log::debug;
use std::time::Duration;

/// Preloader / BROM protocol handler
pub struct Preloader {
    pub device: UsbDevice,
    pub is_preloader_mode: bool,
    pub chip: Option<ChipConfig>,
}

impl Preloader {
    pub fn new(device: UsbDevice) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            chip: None,
        }
    }

    /// 基础初始化（BROM 握手确认设备可用）
    pub fn init(&mut self) -> Result<bool, String> {
        // 发送 0xA0 确认 BROM 就绪
        self.echo(&[0xA0])
    }

    /// BROM echo 协议：发送数据，等待设备回显相同数据
    /// Python: Port.echo(data) — 对每个 val in data 执行 usbwrite(val) + usbread(len(val), maxtimeout=0)
    pub fn echo(&mut self, data: &[u8]) -> Result<bool, String> {
        self.device.set_timeout(Duration::from_millis(1000));
        match self.device.write(data) {
            Ok(_) => {
                let mut buf = vec![0u8; data.len()];
                match self.device.read(&mut buf) {
                    Ok(n) if n == data.len() => {
                        if buf == data {
                            Ok(true)
                        } else {
                            debug!(
                                "[ECHO] data mismatch: expected {:02X?}, got {:02X?}",
                                data, &buf[..n]
                            );
                            Ok(false)
                        }
                    }
                    Ok(n) => {
                        debug!(
                            "[ECHO] length mismatch: expected {} bytes, got {} bytes, data={:02X?}",
                            data.len(),
                            n,
                            &buf[..n]
                        );
                        Ok(false)
                    }
                    Err(e) => {
                        debug!("[ECHO] read error: {}", e);
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                debug!("[ECHO] write error: {}", e);
                Err(e)
            }
        }
    }

    /// 带标签的诊断 echo（用于调试）
    #[allow(dead_code)]
    pub fn echo_debug(&mut self, data: &[u8], label: &str) -> Result<bool, String> {
        debug!(
            "[ECHO:{}] sending {} bytes: {:02X?}",
            label,
            data.len(),
            data
        );
        let result = self.echo(data);
        match &result {
            Ok(true) => debug!("[ECHO:{}] OK, echo matched", label),
            Ok(false) => debug!("[ECHO:{}] failed", label),
            Err(e) => debug!("[ECHO:{}] ERR: {}", label, e),
        }
        result
    }

    /// 发送命令（带 echo 确认）
    pub fn sendcmd(&mut self, cmd: u8) -> Result<bool, String> {
        self.echo(&[cmd])
    }

    /// 获取硬件代码
    pub fn get_hw_code(&mut self) -> Result<u16, String> {
        if !self.sendcmd(0xFD)? {
            return Err("获取 HW code 失败: echo 0xFD 不匹配".into());
        }
        let hw = self.rword()?;
        debug!("HW code: {:04X}", hw);
        Ok(hw)
    }

    /// 获取目标设备安全配置
    pub fn get_target_config(&mut self) -> Result<TargetConfig, String> {
        let hw = self.get_hw_code()?;
        let chip = CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == hw)
            .ok_or_else(|| format!("未知 HW code: {:04X}", hw))?;
        self.chip = Some(chip.clone());
        // 读取安全配置（echo 0xC8）
        self.sendcmd(0xC8).ok();
        let raw = self.rdword()?;
        Ok(TargetConfig::from_raw(raw))
    }

    /// SEND_DA: 发送 Download Agent 到设备
    pub fn send_da(
        &mut self,
        addr: u32,
        data_len: u32,
        sig_len: u32,
        dadata: &[u8],
    ) -> Result<bool, String> {
        // echo 0xD7 确认设备就绪
        if !self.echo(&[0xD7])? {
            return Err("SEND_DA failed: echo 0xD7 不匹配".into());
        }

        // 发送 DA 基址
        self.device
            .write(&addr.to_be_bytes())
            .map_err(|e| format!("SEND_DA write addr err: {}", e))?;

        // 发送 DA 数据长度
        self.device
            .write(&data_len.to_be_bytes())
            .map_err(|e| format!("SEND_DA write data_len err: {}", e))?;

        // 发送签名长度
        self.device
            .write(&sig_len.to_be_bytes())
            .map_err(|e| format!("SEND_DA write sig_len err: {}", e))?;

        // 发送 DA 数据部分
        self.device
            .write(&dadata[..data_len as usize])
            .map_err(|e| format!("SEND_DA write data err: {}", e))?;

        // 发送签名数据
        let sig_start = data_len as usize;
        if sig_len > 0 && sig_start < dadata.len() {
            self.device
                .write(&dadata[sig_start..sig_start + sig_len as usize])
                .map_err(|e| format!("SEND_DA write sig err: {}", e))?;
        }

        // 读状态
        let status = self.rword()?;
        debug!("SEND_DA status: {:04X}", status);
        Ok(status == 0)
    }

    /// JUMP_DA: 跳转到 Download Agent
    pub fn jump_da(&mut self, addr: u32) -> Result<bool, String> {
        // echo 0xD5 确认
        if !self.echo(&[0xD5])? {
            return Err("jump_da: echo 0xD5 不匹配".into());
        }
        // 发送跳转地址
        self.device
            .write(&addr.to_be_bytes())
            .map_err(|e| format!("jump_da write addr err: {}", e))?;
        Ok(true)
    }

    /// JUMP_BL: 跳转到 Bootloader（复位设备）
    pub fn jump_bl(&mut self) -> Result<(), String> {
        if !self.echo(&[0xD8])? {
            return Err("jump_bl: echo 0xD8 不匹配".into());
        }
        Ok(())
    }

    /// BROM 寄存器访问（DA 注入核心操作）
    /// mode=0: 读, mode=1: 写
    /// Python: brom_register_access(address, length, data, check_result)
    pub fn brom_register_access(
        &mut self,
        address: u32,
        length: u32,
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        let mode: u32 = if data.is_some() { 1 } else { 0 };

        if !self.echo(&[0xDA])? {
            return Err("brom_register_access: echo 0xDA 不匹配".into());
        }

        self.device
            .write(&mode.to_be_bytes())
            .map_err(|e| format!("brom_reg write mode: {}", e))?;

        self.device
            .write(&address.to_be_bytes())
            .map_err(|e| format!("brom_reg write addr: {}", e))?;

        self.device
            .write(&length.to_be_bytes())
            .map_err(|e| format!("brom_reg write len: {}", e))?;

        let mut st = [0u8; 2];
        self.device.read_exact(&mut st).ok();
        debug!("brom_reg status1: {:02X?}", st);
        if st != [0, 0] {
            return Err("status err".into());
        }

        if let Some(wdata) = data {
            self.device
                .write(wdata)
                .map_err(|e| format!("brom_reg write data: {}", e))?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device.read_exact(&mut st2).ok();
                debug!("brom_reg status3: {:02X?}", st2);
                if st2 != [0, 0] {
                    return Err("status2 err".into());
                }
            }
            Ok(None)
        } else {
            let rdata = self.rbyte(length as usize)?;
            debug!("brom_reg read data: {:02X?}", rdata);
            let mut st2 = [0u8; 2];
            self.device.read_exact(&mut st2).ok();
            debug!("brom_reg status2: {:02X?}", st2);
            Ok(Some(rdata))
        }
    }

    /// 读 32 位值（BROM 模式）
    pub fn read32_brom(&mut self, addr: u32, dwords: usize) -> Result<Vec<u8>, String> {
        self.brom_register_access(addr, (dwords * 4) as u32, None, true)
            .map(|r| r.unwrap_or_default())
    }

    /// 读多个字节
    pub fn rbyte(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; n];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read_exact {} bytes: {}", n, e))?;
        Ok(buf)
    }

    /// 读 16 位字
    pub fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// 读 32 位双字
    pub fn rdword(&mut self) -> Result<u32, String> {
        let mut buf = [0u8; 4];
        self.device.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }
}