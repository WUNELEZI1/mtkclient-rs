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

mod da_load;
mod diag;
mod emi;
mod io;
pub mod protocol;

pub use protocol::*;

use crate::preloader::Preloader;

/// EMMC 信息结构
pub struct EmmcInfo {
    pub boot1_size: u64,
    pub boot2_size: u64,
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
