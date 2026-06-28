//! Preloader / BROM 协议层
//!
//! - `core`     — Preloader 结构体定义
//! - `transport` — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `brom_init` — init / sync_brom / echo_* / get_hw_code / rword / flush_input / read32_brom
//! - `brom_register_access` — 0xDA 协议通用寄存器访问
//! - `brom_io`   — send_da / jump_da / jump_bl / get_me_id / get_soc_id

pub(crate) mod brom_init;
pub(crate) mod brom_io;
pub(crate) mod brom_register_access;
pub(crate) mod core;
pub(crate) mod transport;

// 公共 API re-export（与原 preloader.rs 完全兼容）
#[allow(unused_imports)]
pub use core::Preloader;
#[allow(unused_imports)]
pub use transport::{BromPortResult, BromTransport, SerialPortTransport};
