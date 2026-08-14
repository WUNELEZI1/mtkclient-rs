//! XFlash 协议原语层
//!
//! 本模块承载 XFlash 协议层最底层的数据包/状态机原语，被 DAXFlash 的所有
//! 高层方法（setup/upload/read/erase/...）复用：
//! - 常量定义（CMD_MAGIC、所有 CMD_* 命令码）
//! - 包头打包（pack3）
//! - 同步/状态读取（xflash_sync、status、xread、xread_data）
//! - ACK 响应（ack、send_ack）
//! - 设备控制命令（send_devctrl、get/set_*_devctrl 包装）
//!
//! 设计原则：保持与 Python mtkclient xflash_lib.py 的协议层一一对应，
//! 不依赖任何上层 DAXFlash 业务逻辑，方便单测与回溯。

use log::{info, trace, warn};
use std::time::Duration;

use crate::da::xflash::DAXFlash;

// =============================================================================
// XFlash 命令常量
// =============================================================================

/// 所有 XFlash 包头的 magic 标识
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
pub const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
pub const CMD_SETUP_ENVIRONMENT: u32 = 0x010100;
pub const CMD_SETUP_HW_INIT_PARAMS: u32 = 0x010101;
pub const CMD_INIT_EXT_RAM: u32 = 0x01000A;
pub const CMD_BOOT_TO: u32 = 0x010008;
pub const CMD_READ_DATA: u32 = 0x010005; // XFlash 读分区命令
pub const CMD_WRITE_DATA: u32 = 0x010004; // 写入数据命令

pub const CMD_FORMAT: u32 = 0x010003; // 格式化命令

// =============================================================================
// XFlash DA 完整命令码表（对齐 MTKAuthPass.exe + mtkclient xflash_lib.py）
// =============================================================================

// --- 基础命令 (0x01xxxx) ---
pub const CMD_SHUTDOWN: u32 = 0x010007; // ★ 关机/reboot（含 bootmode 参数）

// --- 设备控制子命令 (0x02xxxx) — 通过 CMD_DEVICE_CTRL 发送 ---
pub const SET_META_BOOT_MODE: u32 = 0x020006; // ★ 设置 Meta Boot Mode

// --- 信息获取子命令 (0x04xxxx) ---
pub const GET_PKT_LEN: u32 = 0x040007; // 获取包长度

// --- 其他命令 ---
pub const GET_DA_VER_CMD: u32 = 0x010106; // 获取 DA 版本/芯片信息

// --- DA 扩展启动地址 ---
pub const DA_EXT_BOOT_ADDR: u32 = 0x4FFF0000;

// =============================================================================
// 超时常量 (ms)
// =============================================================================

/// xread 临时超时 (ms)
const XREAD_TIMEOUT_MS: u64 = 1000;
/// 可选查询短超时 (ms)
const SHORT_QUERY_TIMEOUT_MS: u64 = 200;
/// SLA 查询超时 (ms)
const SLA_QUERY_TIMEOUT_MS: u64 = 50;

// =============================================================================
// Shutdown bootmode 枚举（对齐 xflash_lib.py ShutDownModes）
// =============================================================================

/// DA Shutdown 命令的 bootmode 参数
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShutdownBootMode {
    /// 关机
    Normal = 0,
}

// =============================================================================
// XML DA 命令协议（Layer 3，新平台 MT6789+）
// =============================================================================

/// XML DA 命令的 BootMode 枚举
#[derive(Debug, Clone, Copy)]
pub enum XmlBootMode {
    Fastboot = 0,
    Meta = 1,
}

impl XmlBootMode {
    fn as_str(&self) -> &'static str {
        match self {
            XmlBootMode::Fastboot => "FASTBOOT",
            XmlBootMode::Meta => "META",
        }
    }
}

/// 构建 XML DA 命令包
///
/// XML 协议格式（新平台 MT6789+ 使用）：
/// ```xml
/// <?xml version="1.0" encoding="utf-8"?>
/// <da>
///   <version>1.0</version>
///   <command>CMD:{命令名}</command>
///   <arg>{参数XML}</arg>
/// </da>
/// ```
fn build_xml_command(cmd_name: &str, arg_content: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
         <da>\r\n\
         <version>1.0</version>\r\n\
         <command>CMD:{}</command>\r\n\
         <arg>\r\n\
         {}\r\n\
         </arg>\r\n\
         </da>",
        cmd_name, arg_content
    )
    .into_bytes()
}

/// 构建 SET-BOOT-MODE XML 命令
///
/// 对齐 mtkclient da_cmd.py cmd_set_boot_mode：
/// ```xml
/// <mode>FASTBOOT</mode>
/// <connect_type>USB</connect_type>
/// <mobile_log>ON</mobile_log>
/// <adb>ON</adb>
/// ```
pub fn xml_set_boot_mode(mode: XmlBootMode) -> Vec<u8> {
    let arg = format!(
        "<mode>{}</mode>\r\n\
         <connect_type>USB</connect_type>\r\n\
         <mobile_log>ON</mobile_log>\r\n\
         <adb>ON</adb>",
        mode.as_str()
    );
    build_xml_command("SET-BOOT-MODE", &arg)
}

/// 构建 REBOOT XML 命令
///
/// 对齐 mtkclient da_cmd.py cmd_reboot：
/// ```xml
/// <action>IMMEDIATE</action>
/// ```
pub fn xml_reboot(disconnect: bool) -> Vec<u8> {
    let action = if disconnect {
        "DISCONNECT"
    } else {
        "IMMEDIATE"
    };
    let arg = format!("<action>{}</action>", action);
    build_xml_command("REBOOT", &arg)
}

// =============================================================================
// 包头工具
// =============================================================================

/// pack3: 生成 XFlash 参数包头 (magic(4) + data_type(4) + length(4))
pub fn pack3(magic: u32, data_type: u32, length: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..8].copy_from_slice(&data_type.to_le_bytes());
    buf[8..12].copy_from_slice(&length.to_le_bytes());
    buf
}

// =============================================================================
// ACK 响应枚举 — 替代裸 u32 返回值
// =============================================================================

/// ACK 响应枚举 — 替代裸 u32 返回值
#[derive(Debug, PartialEq)]
pub enum AckResult {
    /// status == 0，设备就绪，继续传输
    Continue,
    /// status != 0，传输终止或设备报错
    Terminated(u32),
}

// =============================================================================
// 协议原语 — 块 1: 状态 / 数据读取
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    pub(crate) fn xread(&mut self) -> Result<u32, String> {
        // 设备处理 setup_hw_init 后可能需要数百毫秒才返回响应
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(XREAD_TIMEOUT_MS));

        let result = self.xread_inner();
        self.preloader.device.set_timeout(orig_timeout);
        result
    }

    fn xread_inner(&mut self) -> Result<u32, String> {
        // 读取 12 字节的 XFlash 头
        let mut header = [0; 12];
        self.preloader.device.read_exact(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let _data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            trace!("[xread] bad magic: 0x{:08X}", magic);
            return Err(format!("XFlash 头 magic 错误: 0x{:08X}", magic));
        }

        trace!("[xread] dt={:08X} len={}", _data_type, length);

        // 读取数据
        if length > 0 {
            let mut data = vec![0; length as usize];
            self.preloader.device.read_exact(&mut data)?;

            // 如果数据是 4 字节，返回 u32 值
            if length == 4 {
                let val = u32::from_le_bytes(data[0..4].try_into().unwrap());
                trace!("[xread] val=0x{:08X}", val);
                return Ok(val);
            }
        }

        Ok(0)
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read_exact(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let _data_type = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            trace!("[status] bad magic: 0x{:08X}", magic);
            return Err(format!("status magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut tmp = vec![0u8; length as usize];
            self.preloader.device.read_exact(&mut tmp)?;
            if length == 4 {
                let val = u32::from_le_bytes(tmp[..4].try_into().unwrap());
                // Python 特殊情况：如果 status == 0xFEEEEEEF，返回 0
                if val == 0xFEEEEEEF {
                    trace!("[status] 0xFEEEEEEF → 0");
                    return Ok(0);
                }
                trace!(
                    "[status] dt={:08X} len={} val=0x{:08X}",
                    _data_type, length, val
                );
                return Ok(val);
            } else if length == 2 {
                let val = u16::from_le_bytes(tmp[..2].try_into().unwrap()) as u32;
                trace!(
                    "[status] dt={:08X} len={} val=0x{:04X}",
                    _data_type, length, val
                );
                return Ok(val);
            }
            trace!(
                "[status] dt={:08X} len={} (non-u32/u16)",
                _data_type, length
            );
        }
        trace!("[status] dt={:08X} len={} (no payload)", _data_type, length);
        Ok(0)
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read_exact(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let _data_type = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            trace!("[xread_data] bad magic: 0x{:08X}", magic);
            return Err(format!("xread magic error: 0x{:08X}", magic));
        }
        trace!("[xread_data] dt={:08X} len={}", _data_type, length);
        if length > 0 {
            let mut data = vec![0u8; length as usize];
            self.preloader.device.read_exact(&mut data)?;
            Ok(data)
        } else {
            Ok(vec![])
        }
    }
}

// =============================================================================
// 协议原语 — 块 2: ACK 读写
// =============================================================================

impl<'a> DAXFlash<'a> {
    /// 发送 ACK 并读取设备响应
    /// 写入 12B header(CMD_MAGIC + 0x01 + 4) + 4B 零值 → 读取 status
    /// 返回 AckResult::Continue（可继续）或 AckResult::Terminated（终止）
    pub(crate) fn ack(&mut self) -> AckResult {
        if let Err(e) = self.send_ack() {
            trace!("[ack] send_ack failed: {}", e);
            return AckResult::Terminated(1);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => AckResult::Terminated(n),
            Err(_) => AckResult::Terminated(3),
        }
    }

    fn send_ack(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        let zero = 0u32.to_le_bytes();

        // 所有芯片统一使用 16B 单包发送 ACK，避免两次 USB OUT transfer 的调度开销
        let mut buf = [0u8; 16];
        buf[..12].copy_from_slice(&hdr);
        buf[12..16].copy_from_slice(&zero);
        self.preloader
            .device
            .write(&buf)
            .map_err(|e| format!("send_ack write 16B: {}", e))?;
        Ok(())
    }
}

// =============================================================================
// 协议原语 — 块 3: 同步 / devctrl
// =============================================================================

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

// =============================================================================
// 协议原语 — 块 4: SHUTDOWN / XML DA 命令 (Layer 2 + Layer 3)
// =============================================================================

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
    /// bootmode 枚举：
    ///   0 = 关机/重启 (NORMAL) — 配合 enablewdt 触发重启
    ///   1 = 重启到系统 (REBOOT)
    ///   2 = ★ 重启到 fastboot (FASTBOOT)
    pub fn da_shutdown(&mut self, bootmode: ShutdownBootMode) -> Result<(), String> {
        let mode_name = match bootmode {
            ShutdownBootMode::Normal => "关机/重启",
        };
        info!("[DA SHUTDOWN] bootmode={} ({})", bootmode as u32, mode_name);

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
            return Err(format!("SHUTDOWN 命令状态: 0x{:08X}", st));
        }

        // 构建 32 字节参数体 — 严格对齐 mtkclient xflash_lib.py shutdown：
        //   pack("<IIIIIIII", hasflags, enablewdt, async_mode, bootmode,
        //        dl_bit, dont_resetrtc, leaveusb, 0)
        // 关键修正（对比旧实现）：
        //   - enablewdt = 0（禁用看门狗）；重启由 SHUTDOWN 命令本身(bootmode)触发，
        //     旧实现 enablewdt=0x64 与 mtkclient 不符，且部分 DA 会拒绝该命令。
        //   - 参数体 32 字节(8×u32)，旧实现仅 28 字节(缺末尾保留字段)。
        // 字段顺序(均为 u32 LE)：
        //   0x00 hasflags      非 NORMAL 模式 / async / dl_bit 时为 1，否则 0
        //   0x04 enablewdt     0 = 禁用 WDT
        //   0x08 async_mode    0
        //   0x0C bootmode      0 = NORMAL(关机/重启)
        //   0x10 dl_bit        0
        //   0x14 dont_resetrtc 0
        //   0x18 leaveusb      0
        //   0x1C 保留          0
        let async_mode: u32 = 0;
        let dl_bit: u32 = 0;
        let hasflags: u32 = if (bootmode as u32) != 0 || async_mode != 0 || dl_bit != 0 {
            1
        } else {
            0
        };
        let enablewdt: u32 = 0; // 禁用看门狗，由 SHUTDOWN 命令触发重启（对齐 mtkclient）
        let mut param = [0u8; 32];
        param[0x00..0x04].copy_from_slice(&hasflags.to_le_bytes());
        param[0x04..0x08].copy_from_slice(&enablewdt.to_le_bytes());
        param[0x08..0x0C].copy_from_slice(&async_mode.to_le_bytes());
        param[0x0C..0x10].copy_from_slice(&(bootmode as u32).to_le_bytes());
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
            return Err(format!("SHUTDOWN 参数状态: 0x{:08X}", st2));
        }

        info!(
            "[DA SHUTDOWN] 成功 (bootmode={}, enablewdt=0x{:02X})",
            bootmode as u32, enablewdt
        );
        Ok(())
    }


    /// Layer 3: 发送 XML DA 命令（新平台 MT6789+）
    ///
    /// XML 协议通过 USB Bulk 传输 XML 格式命令包。
    /// 发送后读取 status 响应。
    fn send_xml_command(&mut self, xml_data: &[u8]) -> Result<(), String> {
        trace!("[XML DA] 发送 {} 字节 XML 命令", xml_data.len());

        // 发送 XML 数据（按 XFlash 格式打包）
        let pkt = pack3(CMD_MAGIC, 0x01, xml_data.len() as u32);
        self.preloader
            .device
            .write(&pkt)
            .map_err(|e| format!("XML cmd write hdr: {}", e))?;
        self.preloader
            .device
            .write(xml_data)
            .map_err(|e| format!("XML cmd write data: {}", e))?;

        // 读取 status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("XML 命令状态: 0x{:08X}", st));
        }

        Ok(())
    }

    /// Layer 3: 通过 XML DA 协议重启到 fastboot（新平台 MT6789+）
    ///
    /// 流程：先发送 SET-BOOT-MODE(FASTBOOT)，再发送 REBOOT(IMMEDIATE)
    pub fn da_xml_reboot_fastboot(&mut self) -> Result<(), String> {
        info!("[XML DA] SET-BOOT-MODE: FASTBOOT");
        let xml_cmd = xml_set_boot_mode(XmlBootMode::Fastboot);
        self.send_xml_command(&xml_cmd)?;

        info!("[XML DA] REBOOT: IMMEDIATE");
        let xml_reboot_cmd = xml_reboot(false);
        self.send_xml_command(&xml_reboot_cmd)?;

        info!("[XML DA] 设备将重启到 fastboot");
        Ok(())
    }

    /// Layer 3: 通过 XML DA 协议重启到 meta（新平台）
    pub fn da_xml_reboot_meta(&mut self) -> Result<(), String> {
        info!("[XML DA] SET-BOOT-MODE: META");
        let xml_cmd = xml_set_boot_mode(XmlBootMode::Meta);
        self.send_xml_command(&xml_cmd)?;

        info!("[XML DA] REBOOT: IMMEDIATE");
        let xml_reboot_cmd = xml_reboot(false);
        self.send_xml_command(&xml_reboot_cmd)?;

        info!("[XML DA] 设备将重启到 meta");
        Ok(())
    }
}

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}

// =============================================================================
// 单元测试 — 协议原语回归验证
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack3_layout() {
        let pkt = pack3(0xDEADBEEF, 0x12345678, 0x9ABCDEF0);
        // magic
        assert_eq!(pkt[0..4], [0xEF, 0xBE, 0xAD, 0xDE]);
        // data_type
        assert_eq!(pkt[4..8], [0x78, 0x56, 0x34, 0x12]);
        // length
        assert_eq!(pkt[8..12], [0xF0, 0xDE, 0xBC, 0x9A]);
    }

    #[test]
    fn ack_result_equality() {
        assert_eq!(AckResult::Continue, AckResult::Continue);
        assert_ne!(AckResult::Continue, AckResult::Terminated(1));
    }
}
