//! WinUSB 设备打开验证（历史模块）
//!
//! 原 `get_device_instance_id`（通过 pnputil 枚举 USB 设备实例 ID）为兼容旧接口的
//! 遗留实现，全平台已无任何调用点（现由 nusb 直接打开设备完成验证，见 switch.rs
//! 的 `poll_libusb_ready`），故已按“显式废弃的死代码直接删除”原则移除。
