//! DA/XFlash 系统
//!
//! 负责 DA (Download Agent) 上传、XFlash 协议通信、分区操作
//!
//! # 系统架构
//! - `protocol` - XFlash 协议原语（pack3、xread、status、ack）
//! - `emi` - EMI 数据提取
//! - `da_load` - DA 加载主流程
//! - `diag` - 设备诊断与信息获取
//! - `io` - 分区读写操作

#[path = "da_load.rs"]
mod da_load;
#[path = "diag.rs"]
mod diag;
#[path = "emi.rs"]
mod emi;
#[path = "io.rs"]
mod io;
#[path = "protocol.rs"]
pub mod protocol;

pub use protocol::*;

use crate::preloader::Preloader;

/// EMMC 完整信息结构（来自 send_devctrl(0x01010C)）
pub struct EmmcInfo {
    /// Boot1 大小（字节）
    pub boot1_size: u64,
    /// Boot2 大小（字节）
    pub boot2_size: u64,
    /// RPMB 大小（字节），如可用
    pub rpmb_size: u64,
    /// 用户数据区总大小（字节）
    pub user_size: u64,
    /// 块大小（字节）
    pub block_size: u32,
    /// EMMC 类型字符串描述
    pub emmc_type: String,
    /// CID 寄存器内容（16 字节）
    pub cid: Vec<u8>,
}

impl EmmcInfo {
}

/// DAXFlash 结构体，处理 XFlash 协议
pub struct DAXFlash<'a> {
    pub preloader: &'a mut Preloader,
    /// EMI 数据 — 由 emi 模块的 `load_preloader_emi` 写入
    pub(crate) emi: Option<Vec<u8>>,
    pub(crate) emi_version: u32,
    pub(crate) da2_data: Vec<u8>,
    pub(crate) da2_base_addr: u64,
    pub daext: bool,
    pub(crate) last_gpt_data: Option<Vec<u8>>,
    pub patch_da: bool,
    pub da_x_speed: u8,
    /// 缓存 DA 文件数据，避免 upload_da1/da2 重复读取
    pub(crate) da_file_data: Option<Vec<u8>>,
    /// 当前使用的 preloader 文件路径，用于保存到 .state
    pub(crate) preloader_path: Option<String>,
    /// 当前进程内记录的可选 DA 查询失败项，DA 会话保存后同步到 .state
    pub(crate) optional_query_failures: Vec<String>,
    /// 动态分区 (super) 元数据缓存
    pub(crate) super_metadata: Option<crate::partition::lp::SuperMetadata>,
}

impl<'a> DAXFlash<'a> {
    pub fn new(preloader: &'a mut Preloader) -> Self {
        DAXFlash {
            preloader,
            emi: None,
            emi_version: 0,
            da2_data: Vec::new(),
            da2_base_addr: 0x40000000,
            daext: false,
            last_gpt_data: None,
            super_metadata: None,
            patch_da: true,
            da_x_speed: 3,
            da_file_data: None,
            preloader_path: None,
            optional_query_failures: Vec::new(),
        }
    }
}
