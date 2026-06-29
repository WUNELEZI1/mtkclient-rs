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

#[path = "加载.rs"]
mod da_load;
#[path = "诊断.rs"]
mod diag;
#[path = "内存初始化.rs"]
mod emi;
#[path = "输入输出.rs"]
mod io;
#[path = "协议.rs"]
pub mod protocol;

pub use protocol::*;

use crate::预加载器::Preloader;

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
    /// 是否为合法 EMMC
    pub fn is_valid(&self) -> bool {
        self.user_size > 0
    }
    /// Boot1 大小（MB）
    pub fn boot1_size_mb(&self) -> u64 {
        self.boot1_size / 1024 / 1024
    }
    /// Boot2 大小（MB）
    pub fn boot2_size_mb(&self) -> u64 {
        self.boot2_size / 1024 / 1024
    }
    /// 用户区大小（MB）
    pub fn user_size_mb(&self) -> u64 {
        self.user_size / 1024 / 1024
    }
    /// 用户区大小（GB）
    pub fn user_size_gb(&self) -> u64 {
        self.user_size / 1024 / 1024 / 1024
    }
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
            patch_da: true,
        }
    }
}
