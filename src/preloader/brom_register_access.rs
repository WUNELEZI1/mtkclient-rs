//! BROM 寄存器访问（DA 注入核心操作）
//!
//! - `brom_register_access` — 通用 0xDA 协议（read/write）
//! - 重试 / clear_halt 容错逻辑
//!
//! 协议：cmd(D1/DA) → mode(4B) → address(4B) → length_dwords(4B) → status(2B) → data → status(2B)
//! mode: 0=read, 1=write
//! length_dwords: DWORD 数（设备期望的单位）

use super::core::Preloader;
use log::trace;
use std::time::Duration;

const BROM_REG_READ_TIMEOUT_MS: u64 = 10000;
const BROM_REG_RETRY_ATTEMPTS: usize = 4;
const BROM_REG_RETRY_DELAY_MS: u64 = 100;

impl Preloader {
    /// BROM 寄存器访问（DA 注入核心操作）
    pub fn brom_register_access(
        &mut self,
        mode: u32, // 0=读，1=写
        address: u32,
        length_bytes: u32, // 字节数，对齐 Python brom_register_access
        data: Option<&[u8]>,
        check_status: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        // 命令字节 0xDA：使用 echo 协议（对齐 Python Cmd.brom_register_access.value）
        // 注意：Python 不检查 echo 返回值，这里也只记录警告不返回错误
        if !self.echo_1byte(0xDA)? {
            trace!("brom_reg: echo 0xDA 不匹配（继续执行）");
        }
        if !self.echo_4byte(mode)? {
            trace!("brom_reg: echo mode 不匹配（继续执行）");
        }
        if !self.echo_4byte(address)? {
            trace!("brom_reg: echo addr 不匹配（继续执行）");
        }
        if !self.echo_4byte(length_bytes)? {
            trace!("brom_reg: echo len 不匹配（继续执行）");
        }

        // 读状态 2 字节（容错模式，对齐 Python 的宽松风格）
        let mut st = [0u8; 2];
        if let Err(e) = self.device.read_exact(&mut st) {
            trace!("brom_reg status1 read failed: {}, continue", e);
        } else {
            trace!("brom_reg status1: {:02X?}", st);
        }

        if let Some(wdata) = data {
            // Write mode: 发送 length_bytes 字节数据后读 status2
            let byte_count = length_bytes as usize;
            self.device
                .write(&wdata[..byte_count])
                .map_err(|e| format!("brom_reg write data: {}", e))?;
            if check_status {
                let mut st2 = [0u8; 2];
                self.device
                    .read_exact(&mut st2)
                    .map_err(|e| format!("brom_reg status2: {}", e))?;
                trace!("brom_reg status2: {:02X?}", st2);
            }
            Ok(None)
        } else {
            // Read mode: 读取 length_bytes 字节
            let byte_count = length_bytes as usize;
            let mut buf = vec![0u8; byte_count];

            trace!("brom_reg 准备读 {} 字节数据", byte_count);

            // 关键修复：增加超时 + 重试 + clear_halt
            self.device
                .set_timeout(Duration::from_millis(BROM_REG_READ_TIMEOUT_MS));
            let _ = self.device.clear_halt_in();

            let mut success = false;
            for attempt in 0..BROM_REG_RETRY_ATTEMPTS {
                match self.device.read_exact(&mut buf) {
                    Ok(n) if n == byte_count => {
                        trace!("brom_reg read data 成功: {} 字节", n);
                        success = true;
                        break;
                    }
                    _ => {
                        trace!("brom_reg read attempt {} failed, flush...", attempt + 1);
                        self.flush_input();
                        std::thread::sleep(Duration::from_millis(BROM_REG_RETRY_DELAY_MS));
                    }
                }
            }

            if !success {
                return Err("brom_reg read data timeout after retries".into());
            }

            // 读 status2
            let mut st2 = [0u8; 2];
            let _ = self.device.read_exact(&mut st2);
            trace!("brom_reg status2: {:02X?}", st2);

            Ok(Some(buf))
        }
    }
}
