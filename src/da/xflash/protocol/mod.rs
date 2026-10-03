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

// =============================================================================
// XFlash 命令常量
// =============================================================================

/// 所有 XFlash 包头的 magic 标识
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
/// XFlash 包头 `data_type`：正常流程包（命令响应 / 数据）。
/// 参考 penumbra `DataType::Flow`。
pub const DATA_TYPE_FLOW: u32 = 0x1;
/// XFlash 包头 `data_type`：设备异步消息包（DA 日志 / 状态通告）。
/// 参考 penumbra `DataType::Message`。DA 可在任意时刻插入此类包，
/// 若不消费其负载会导致后续包头错位（协议失步）。
pub const DATA_TYPE_MESSAGE: u32 = 0x2;
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
// Shutdown bootmode（对齐 xflash_lib.py ShutDownModes）
// =============================================================================

/// DA SHUTDOWN 的 bootmode 取值（对齐 mtkclient xflash_lib.py ShutDownModes）。
///
/// 本工程经 DA SHUTDOWN 重启时统一使用 HOME_SCREEN（=1，重启到系统）：
/// fastboot / recovery / fastbootd / meta 等目标由调用方先写 misc/para 或改走
/// XML SET-BOOT-MODE，再以 HOME_SCREEN 重新拉起系统由 LK 读取启动参数，无需
/// DA 端的其它 bootmode。
pub const SHUTDOWN_BOOTMODE_HOME_SCREEN: u32 = 1;

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

/// 写入分块的 16-bit additive checksum（对齐 penumbra `download_data`）。
///
/// DA 在每块数据前期望一个 16 位累加和：分块所有字节按 u32 累加后取低 16 位。
/// `& 0xFFFF` 是关键——旧实现用未截断的 32 位 wrapping sum，DA 端只比对低 16 位，
/// 当累加和高位非零时会被判定为校验失败。按 8 字节批量累加以减少循环次数。
pub fn chunk_checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 8 <= data.len() {
        sum = sum
            .wrapping_add(data[i] as u32)
            .wrapping_add(data[i + 1] as u32)
            .wrapping_add(data[i + 2] as u32)
            .wrapping_add(data[i + 3] as u32)
            .wrapping_add(data[i + 4] as u32)
            .wrapping_add(data[i + 5] as u32)
            .wrapping_add(data[i + 6] as u32)
            .wrapping_add(data[i + 7] as u32);
        i += 8;
    }
    while i < data.len() {
        sum = sum.wrapping_add(data[i] as u32);
        i += 1;
    }
    sum & 0xFFFF
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

    #[test]
    fn chunk_checksum_is_additive_16bit() {
        assert_eq!(chunk_checksum(&[]), 0);
        assert_eq!(chunk_checksum(&[0x01]), 0x01);
        assert_eq!(chunk_checksum(&[0xFF, 0x01]), 0x100);
        // 与朴素实现一致（含跨越 8 字节批量边界的样本）
        let naive = |d: &[u8]| d.iter().map(|&b| b as u32).sum::<u32>() & 0xFFFF;
        let sample: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        assert_eq!(chunk_checksum(&sample), naive(&sample));
        // 长度不是 8 的整数倍时尾部也要计入
        assert_eq!(chunk_checksum(&[0u8; 17]), 0);
        assert_eq!(chunk_checksum(&[0u8; 16]), 0);
    }

    #[test]
    fn chunk_checksum_truncates_to_16_bits() {
        // 255 * 300 = 76500 = 0x12AD4 → 低 16 位 0x2AD4（旧实现会返回 0x12AD4）
        let data = vec![0xFFu8; 300];
        assert_eq!(chunk_checksum(&data), 0x2AD4);
        assert!(chunk_checksum(&data) <= 0xFFFF);
    }
}
