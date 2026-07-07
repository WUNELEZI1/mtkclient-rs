//! Preloader 核心结构体
//!
//! ```text
//! pub struct Preloader {
//!     pub device: Box<dyn BromTransport>, // 设备传输抽象（USB / 串口）
//!     pub is_preloader_mode: bool,         // 是否已处于 Preloader/DA 模式
//!     pub chip: Option<ChipConfig>,        // 芯片配置（HW code 匹配后填入）
//!     pub brom_initialized: bool,          // BROM init 是否成功（握手 + 关看门狗 + sync）
//! }
//! ```
//!
//! 各 impl 方法分散到同目录其他模块：
//! - `传输.rs`         — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `BROM初始化.rs`         — init / sync_brom / echo_* / get_hw_code / rword / flush_input
//! - `BROM寄存器访问.rs` — brom_register_access
//! - `BROM输入输出.rs`           — send_da / jump_da / jump_bl / get_me_id / get_soc_id
//! - `BROM初始化.rs` 中也包含 read32_brom（因与 rword 等基础读取紧密相关）

use super::transport::BromTransport;
use crate::da::loader::header::DaRegion;
use crate::system::config::ChipConfig;
use std::sync::{Arc, Mutex};

/// DA 文件后台预解析结果
#[derive(Clone)]
pub struct DAPreparseResult {
    pub da_data: Vec<u8>,
    pub magic: u32,
    pub regions: Vec<DaRegion>,
    pub is_v6: bool,
}

pub struct Preloader {
    pub device: Box<dyn BromTransport>,
    pub is_preloader_mode: bool,
    pub chip: Option<ChipConfig>,
    /// BROM 初始化是否完成（init 成功后为 true）
    pub brom_initialized: bool,
    /// 后台线程预解析的 DA 文件结果（init 获取 HW code 后启动线程填充）
    pub da_preparse: Option<Arc<Mutex<Option<DAPreparseResult>>>>,
}

impl Preloader {
    pub fn new(device: Box<dyn BromTransport>) -> Self {
        Preloader {
            device,
            is_preloader_mode: false,
            chip: None,
            brom_initialized: false,
            da_preparse: None,
        }
    }
}
