//! Windows 驱动层 — WinUSB 驱动安装 + 管理员权限检测
//!
//! 驱动切换流程（Zadig 风格，不卸载设备、不需要重新插拔）：
//! 1. libusb 能打开设备     → 已就绪，直接返回
//! 2. wdi-rs create_list     → 找到 BROM 设备（device info）
//! 3. wdi-rs prepare_driver  → 生成 + 自签名 + 注册 WinUSB INF 到驱动商店
//! 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
//!    → 强制覆盖 wdm_usb（绕过 libwdi 预检）
//! 5. libusb 实际打开验证   → 唯一真相
//!
//! 关键点：
//! - wdi-rs 高层 install() 会先 check_existing_driver → 设备有 wdm_usb 时
//!   返回 Error::Exists 且**不**生成 INF。
//! - 但 wdi-rs 公开了 prepare_driver（wdi.rs），它**不**做 exists 检查，
//!   直接生成 + 签名 INF 并加入驱动商店。
//! - 然后用 raw FFI 调 UpdateDriverForPlugAndPlayDevicesW + INSTALLFLAG_FORCE
//!   强制安装已准备好的 INF。Zadig 就是这个套路。

#[path = "admin.rs"]
pub(crate) mod admin;
#[path = "detect.rs"]
pub(crate) mod detect;
#[path = "setupapi.rs"]
pub(crate) mod setupapi;
#[path = "switch.rs"]
pub(crate) mod switch;
#[path = "verify.rs"]
pub(crate) mod verify;

// 公共 API re-export（与原 driver.rs 完全兼容）
// 保留 re-export 是为了外部模块可以通过 `conn_mgr::driver::xxx` 直接调用
#[allow(unused_imports)]
pub use admin::{is_admin, restart_as_admin};
#[allow(unused_imports)]
pub use detect::{
    BromDriverType, ComPortUsbInfo, UsbBusDetectionResult, check_brom_driver_type,
    detect_brom_driver_from_usb_bus, query_com_port_usb_info,
};
#[allow(unused_imports)]
pub use switch::{ensure_winusb_driver, install_winusb_with_wdi, switch_to_winusb};
#[allow(unused_imports)]
pub use verify::{check_winusb_installed, get_device_instance_id};
