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

pub(crate) mod ack;
pub(crate) mod devctrl;
pub(crate) mod read;
pub(crate) mod shutdown;
pub(crate) mod xml;

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
pub const SET_PKT_LEN: u32 = 0x040008; // 设置包长度（抬升批量传输单元，对齐 GeekFlashTool 2MB 块）

// --- 其他命令 ---
pub const GET_DA_VER_CMD: u32 = 0x010106; // 获取 DA 版本/芯片信息

// --- DA 扩展启动地址 ---
pub const DA_EXT_BOOT_ADDR: u32 = 0x4FFF0000;

// =============================================================================
// 超时常量 (ms)
// =============================================================================

/// DA 控制读（status / xread / xread_data）等待首字节的最长超时 (ms)。
/// 串口默认读超时仅 `SERIAL_OPEN_TIMEOUT_MS`(1s)，而慢速 Preloader 串口 DA
/// 在收到重命令（GET_PKT_LEN、setup_hw、send_devctrl、write status 等）后回包
/// 可能超 1s，导致 status 读取 `Operation timed out` 而命令失败。故 DA 控制读统一
/// 用较长超时等待首字节。USB 路径读为自建循环，长超时同样安全。
/// 注意：与 `with_short_timeout` 的快速失败协同——若调用方已显式设更短的超时
///（如可选查询 200ms/50ms），以 `max(当前, 本值)` 取较大者，避免覆盖快速失败语义。
const DA_CTRL_TIMEOUT_MS: u64 = 5000;
/// 可选查询短超时 (ms)
const SHORT_QUERY_TIMEOUT_MS: u64 = 200;
/// SLA 查询超时 (ms)
const SLA_QUERY_TIMEOUT_MS: u64 = 50;

// =============================================================================
// Shutdown bootmode 枚举（对齐 xflash_lib.py ShutDownModes）
// =============================================================================

/// DA Shutdown 命令的 bootmode 参数（对齐 mtkclient xflash_lib.py ShutDownModes）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShutdownBootMode {
    /// 标准关机/重启（Standard shutdown，对齐 mtkclient NORMAL=0）
    #[allow(dead_code)] // 协议保留值：供 da_power_off / --via da 等未来路径使用
    Normal = 0,
    /// 重启到系统 (HOME_SCREEN / home screen)
    Reboot = 1,
    /// ★ 重启到 fastboot
    #[allow(dead_code)] // 协议保留值：供 --via da fastboot 路径未来使用
    Fastboot = 2,
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
// 私有工具
// =============================================================================

fn hex_str(data: &[u8]) -> String {
    crate::util::hex_str(data)
}

// =============================================================================
// 对外再导出（保持 crate::da::xflash::protocol::* 可达性）
// =============================================================================

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
