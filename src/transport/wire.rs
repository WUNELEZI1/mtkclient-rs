//! MTK 线协议常量（BROM + XFlash），由 `da::engine`（主机侧）与 `transport::vdev`（设备侧）共用。
//!
//! 这里只放**数值**，不放任何逻辑，因此不违反依赖方向 `core ← transport ← da`。
//! 数值本身必须与 `da::xflash::protocol` 中已有的实现一致，
//! 由本模块末尾的一致性测试（conformance test）强制保证：
//! 若两侧常量漂移，测试立即失败。

// =============================================================================
// BROM 层
// =============================================================================

/// BROM 握手字节序列（逐字节发送，设备回按位取反）。
pub const BROM_HANDSHAKE: [u8; 4] = [0xA0, 0x0A, 0x50, 0x05];

/// GET_HW_CODE：回 4 字节 BE（hw_code, hw_sub_code）
pub const BROM_GET_HW_CODE: u8 = 0xFD;
/// GET_TARGET_CONFIG：回 4 字节 BE 配置 + 2 字节 BE status
pub const BROM_GET_TARGET_CONFIG: u8 = 0xD8;
/// BROM 同步：回 8 字节 hw info
pub const BROM_SYNC: u8 = 0xFC;
/// SEND_DA：4 字节地址/长度参数 + 数据体
pub const BROM_SEND_DA: u8 = 0xD7;
/// JUMP_DA：4 字节跳转地址（发两次，第二次触发）
pub const BROM_JUMP_DA: u8 = 0xD5;
/// SEND_DA 被 SLA 拒绝时的状态字
pub const BROM_STATUS_SLA_REQUIRED: u16 = 0x1D0D;
/// JUMP_DA 成功后 DA 发出的同步字节
pub const BROM_JUMP_ACK: u8 = 0xC0;
/// BROM sync FE 的应答字节（实测为 0x03）
pub const BROM_SYNC_FE_ACK: u8 = 0x03;

/// 仿真专用：exploit 握手命令（真机不存在，仅 `transport::vdev` 识别）。
///
/// 真机上的 kamakiri2 / carbonara 是时序敏感的 USB 控制传输序列，无法在纯
/// 字节流仿真里复现；仿真设备用一条显式命令表示"该 exploit 已被设备侧接受"，
/// 以便验证"规划 → 执行 → 能力提升"这条链路本身。
pub const SIM_EXPLOIT_CMD: u8 = 0x1F;

// =============================================================================
// XFlash（DA）层
// =============================================================================

/// 包头 magic
pub const XFLASH_MAGIC: u32 = 0xFEEEEEEF;
/// data_type：流程包
pub const XFLASH_TYPE_FLOW: u32 = 0x1;
/// data_type：设备异步消息（日志）
pub const XFLASH_TYPE_MESSAGE: u32 = 0x2;

/// 包头长度
pub const XFLASH_HEADER_LEN: usize = 12;

/// DA 初始化完成信号（设备主动发送）
pub const XFLASH_SYNC_SIGNAL: u32 = 0x434E5953;

/// 命令：READ_DATA（读 flash）
pub const XFLASH_CMD_READ_DATA: u32 = 0x010005;
/// 命令：WRITE_DATA（写 flash）
pub const XFLASH_CMD_WRITE_DATA: u32 = 0x010004;
/// 命令：FORMAT（擦除 / 格式化）
pub const XFLASH_CMD_FORMAT: u32 = 0x010003;
/// 命令：DEVICE_CTRL 前缀
pub const XFLASH_CMD_DEVICE_CTRL: u32 = 0x010009;
/// 命令：GET_DA_VER
pub const XFLASH_CMD_GET_DA_VER: u32 = 0x010106;
/// 子命令：eMMC 信息查询（mtkclient `Cmd.GET_EMMC_INFO = 0x040001`，高位补零后同值）
pub const XFLASH_CMD_EMMC_INFO: u32 = 0x0004_0001;
/// devctrl：DA extensions CUSTOM_ACK
pub const XFLASH_DEVCTRL_ACK: u32 = 0x0F0000;
/// devctrl：CUSTOM_SET_STORAGE
pub const XFLASH_DEVCTRL_SET_STORAGE: u32 = 0x0F0005;
/// devctrl：CC_OPTIONAL_DOWNLOAD_ACT（写完成后必须发送，否则 DA 认为事务未结束）
///
/// 数值取自 mtkclient `Library/DA/xflash/xflash_param.py`：
/// `CC_OPTIONAL_DOWNLOAD_ACT = 0x080005`。
pub const XFLASH_DEVCTRL_CC_OPTIONAL_DOWNLOAD_ACT: u32 = 0x08_0005;

/// 写入参数块长度（storage/parttype/addr/size + 32 字节 NandExtension）
pub const XFLASH_WRITE_PARAM_LEN: usize = 56;
/// 擦除（FORMAT）参数块长度：同上
pub const XFLASH_FORMAT_PARAM_LEN: usize = 56;
/// 参数分块大小（对齐 mtkclient `send_param`：payload 按 0x200 分块发送）
pub const XFLASH_PARAM_CHUNK: usize = 0x200;

/// FORMAT 进行中
pub const XFLASH_STATUS_CONTINUE: u32 = 0x4004_0004;
/// FORMAT 完成
pub const XFLASH_STATUS_COMPLETE: u32 = 0x4004_0005;
/// 写入校验和错误（设备侧拒绝该分块）
pub const XFLASH_STATUS_CHECKSUM: u32 = 0xC003_0006;
/// DA extensions ACK 期望的 magic
pub const XFLASH_ACK_MAGIC: u32 = 0xA1A2A3A4;
/// 不支持的命令状态字
pub const XFLASH_STATUS_UNSUPPORTED: u32 = 0xC001_0004;

/// READ_DATA 参数块长度（56 字节，其余补零）
pub const XFLASH_READ_PARAM_LEN: usize = 56;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::protocol as p;

    #[test]
    fn constants_match_legacy_protocol_layer() {
        // 主机侧（engine）与既有 DAXFlash 实现必须使用同一套命令码，
        // 否则仿真通过、真机失败。
        assert_eq!(XFLASH_MAGIC, p::CMD_MAGIC);
        assert_eq!(XFLASH_TYPE_FLOW, p::DATA_TYPE_FLOW);
        assert_eq!(XFLASH_TYPE_MESSAGE, p::DATA_TYPE_MESSAGE);
        assert_eq!(XFLASH_CMD_READ_DATA, p::CMD_READ_DATA);
        assert_eq!(XFLASH_CMD_WRITE_DATA, p::CMD_WRITE_DATA);
        assert_eq!(XFLASH_CMD_FORMAT, p::CMD_FORMAT);
        assert_eq!(XFLASH_CMD_DEVICE_CTRL, 0x010009);
        assert_eq!(XFLASH_CMD_GET_DA_VER, p::GET_DA_VER_CMD);
        assert_eq!(XFLASH_SYNC_SIGNAL, p::CMD_SYNC_SIGNAL);
    }

    #[test]
    fn handshake_bytes_are_mtk_standard() {
        assert_eq!(BROM_HANDSHAKE, [0xA0, 0x0A, 0x50, 0x05]);
        // 设备应答为逐字节取反
        assert_eq!(BROM_HANDSHAKE.map(|b| !b), [0x5F, 0xF5, 0xAF, 0xFA]);
    }
}
