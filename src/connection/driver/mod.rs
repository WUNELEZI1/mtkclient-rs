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
// WinUSB 切换依赖 wdi-rs（仅 Windows 且需 libwdi 原生库）：
// - Windows + `winusb-driver` 特性（默认开启）→ 真实实现
// - 其他情况 → 占位实现，保证 crate 可在非 Windows / 无 libwdi 工具链时编译与跑测试
#[cfg(all(target_os = "windows", feature = "winusb-driver"))]
#[path = "switch.rs"]
pub(crate) mod switch;
#[cfg(not(all(target_os = "windows", feature = "winusb-driver")))]
#[path = "switch_stub.rs"]
pub(crate) mod switch;
// verify 模块内容（旧 get_device_instance_id 接口）已废弃删除，保留模块仅为文档留痕。
#[path = "verify.rs"]
pub(crate) mod verify;

// 公共 API re-export（与原 driver.rs 完全兼容）
// 保留 re-export 是为了外部模块可以通过 `connection::driver::xxx` 直接调用；
// 各条目按其定义所在的编译条件同步做 cfg 门控，避免非 Windows / 非默认特性下解析失败。

// is_admin / restart_as_admin 仅在 Windows 定义，调用点（main.rs）也在 Windows 分支内。
#[cfg(target_os = "windows")]
pub use admin::{is_admin, restart_as_admin};

// ComPortUsbInfo / UsbBusDetectionResult / detect_brom_driver_from_usb_bus /
// query_com_port_usb_info 在所有平台都存在；其中后三者被外部模块经本 re-export 调用，
// ComPortUsbInfo 仅作为签名类型被导出，保留兼容导出，故允许未使用导入。
#[allow(unused_imports)]
pub use detect::{
    ComPortUsbInfo, UsbBusDetectionResult, detect_brom_driver_from_usb_bus, query_com_port_usb_info,
};
// BromDriverType 仅在 Windows 或测试构建中定义，re-export 同步门控。
#[cfg(any(target_os = "windows", test))]
#[allow(unused_imports)]
pub use detect::BromDriverType;
// check_brom_driver_type 仅在 Windows + `winusb-driver` 下定义（供 switch.rs 使用），
// 外部无调用点，保留兼容导出故允许未使用导入。
#[cfg(all(target_os = "windows", feature = "winusb-driver"))]
#[allow(unused_imports)]
pub use detect::check_brom_driver_type;

pub use switch::switch_to_winusb;
