//! Preloader / BROM 协议层
//!
//! - `核心`     — Preloader 结构体定义
//! - `传输` — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `BROM初始化` — init / sync_brom / echo_* / get_hw_code / rword / flush_input / read32_brom
//! - `BROM寄存器访问` — 0xDA 协议通用寄存器访问
//! - `BROM输入输出`   — send_da / jump_da / jump_bl / get_me_id / get_soc_id

#[path = "brom_init.rs"]
pub(crate) mod brom_init;
#[path = "brom_io.rs"]
pub(crate) mod brom_io;
#[path = "brom_reg_access.rs"]
pub(crate) mod brom_reg_access;
#[path = "core.rs"]
pub(crate) mod core;
#[path = "transport.rs"]
pub(crate) mod transport;

// 公共 API re-export（与原 preloader.rs 完全兼容）
#[allow(unused_imports)]
pub use core::Preloader;
#[allow(unused_imports)]
pub use transport::{BromPortResult, BromTransport, SerialPortTransport};
