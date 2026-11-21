//! Windows driver layer — WinUSB driver installation + admin privilege detection
//!
//! Driver switch flow (Zadig-style; no device uninstall, no replug required):
//! 1. libusb can open device     → already ready, return immediately
//! 2. wdi-rs create_list         → find the BROM device (device info)
//! 3. wdi-rs prepare_driver      → generate + self-sign + register WinUSB INF to driver store
//! 4. UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)
//!    → force overwrite wdm_usb (bypassing libwdi precheck)
//! 5. libusb actual open verify   → the only source of truth
//!
//! Key points:
//! - wdi-rs high-level install() first calls check_existing_driver → when the device has wdm_usb
//!   it returns Error::Exists and does **not** generate an INF.
//! - But wdi-rs exposes prepare_driver (wdi.rs), which does **not** do an exists check,
//!   and directly generates + signs the INF and adds it to the driver store.
//! - Then use raw FFI to call UpdateDriverForPlugAndPlayDevicesW + INSTALLFLAG_FORCE
//!   to force-install the prepared INF. This is exactly what Zadig does.

#[path = "admin.rs"]
pub(crate) mod admin;
#[path = "detect.rs"]
pub(crate) mod detect;
#[path = "setupapi.rs"]
pub(crate) mod setupapi;
// WinUSB switching depends on wdi-rs (Windows only, requires the libwdi native library):
// - Windows + `winusb-driver` feature (enabled by default) → real implementation
// - otherwise → stub implementation, so the crate compiles and tests run without Windows / libwdi toolchain
#[cfg(all(target_os = "windows", feature = "winusb-driver"))]
#[path = "switch.rs"]
pub(crate) mod switch;
#[cfg(not(all(target_os = "windows", feature = "winusb-driver")))]
#[path = "switch_stub.rs"]
pub(crate) mod switch;
// The verify module contents (old get_device_instance_id interface) are deprecated and removed; the module is kept only for documentation.
#[path = "verify.rs"]
pub(crate) mod verify;

// Public API re-exports (fully compatible with the former driver.rs)
// Re-exports are kept so external modules can call `connection::driver::xxx` directly;
// each item is cfg-gated to match its defining build condition, avoiding resolution failures on non-Windows / non-default features.

// is_admin / restart_as_admin are defined only on Windows, and their call site (main.rs) is also inside a Windows branch.
#[cfg(target_os = "windows")]
pub use admin::{is_admin, restart_as_admin};

// ComPortUsbInfo / UsbBusDetectionResult / detect_brom_driver_from_usb_bus /
// query_com_port_usb_info exist on all platforms; the latter three are called by external
// modules via this re-export, ComPortUsbInfo is exported only as a signature type and kept
// for compatibility, hence unused imports are allowed.
#[allow(unused_imports)]
pub use detect::{
    ComPortUsbInfo, UsbBusDetectionResult, detect_brom_driver_from_usb_bus, query_com_port_usb_info,
};
// BromDriverType is defined only on Windows or in test builds; the re-export is gated accordingly.
#[cfg(any(target_os = "windows", test))]
#[allow(unused_imports)]
pub use detect::BromDriverType;
// check_brom_driver_type is defined only under Windows + `winusb-driver` (used by switch.rs);
// there are no external call sites, and it is kept for compatibility so unused imports are allowed.
#[cfg(all(target_os = "windows", feature = "winusb-driver"))]
#[allow(unused_imports)]
pub use detect::check_brom_driver_type;

pub use switch::switch_to_winusb;
