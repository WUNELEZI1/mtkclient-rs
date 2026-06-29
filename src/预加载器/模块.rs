//! Preloader / BROM 协议层
//!
//! - `核心`     — Preloader 结构体定义
//! - `传输` — BromTransport trait + SerialPortTransport + UsbDevice impl
//! - `BROM初始化` — init / sync_brom / echo_* / get_hw_code / rword / flush_input / read32_brom
//! - `BROM寄存器访问` — 0xDA 协议通用寄存器访问
//! - `BROM输入输出`   — send_da / jump_da / jump_bl / get_me_id / get_soc_id

#[path = "BROM初始化.rs"]
pub(crate) mod BROM初始化;
#[path = "BROM输入输出.rs"]
pub(crate) mod BROM输入输出;
#[path = "BROM寄存器访问.rs"]
pub(crate) mod BROM寄存器访问;
#[path = "核心.rs"]
pub(crate) mod 核心;
#[path = "传输.rs"]
pub(crate) mod 传输;

// 公共 API re-export（与原 preloader.rs 完全兼容）
#[allow(unused_imports)]
pub use 核心::Preloader;
#[allow(unused_imports)]
pub use 传输::{BromPortResult, BromTransport, SerialPortTransport};
