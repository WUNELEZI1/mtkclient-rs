//! DA/XML 协议（V6）支持
//!
//! 适用于现代 Dimensity 芯片（MT6983/MT6855/MT6895/MT6897/MT6789/MT6886/MT6985 等）
//! 对齐 Python mtkclient Library/DA/xml/xml_lib.py
//!
//! 与 XFlash (V5) 的主要区别：
//! - 通信格式为 XML 而非二进制 pack3
//! - Magic = 0xFEEEEEEF
//! - DA 上传流程不同
//! - 分区读写命令格式不同

pub mod protocol;
pub mod da_load;
pub mod io;

// protocol 子模块中的类型可通过 crate::da::xml::protocol 访问

use crate::preloader::Preloader;

/// DA/XML 核心结构体（对齐 DAXFlash）
pub struct DAXML<'a> {
    pub preloader: &'a mut Preloader,
    pub(crate) da2_data: Vec<u8>,
    pub(crate) da2_base_addr: u64,
    /// 动态分区 (super) 元数据缓存
    pub(crate) super_metadata: Option<crate::partition::lp::SuperMetadata>,
    /// 当前使用的 DA 文件路径
    pub(crate) da_path: Option<String>,
    /// 是否已完成初始化
    pub(crate) initialized: bool,
}

impl<'a> DAXML<'a> {
    pub fn new(preloader: &'a mut Preloader) -> Self {
        DAXML {
            preloader,
            da2_data: Vec::new(),
            da2_base_addr: 0x40000000,
            super_metadata: None,
            da_path: None,
            initialized: false,
        }
    }
}
