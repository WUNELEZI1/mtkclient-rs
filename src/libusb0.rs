//! libusb-win32 FFI 绑定
//!
//! 使用 libusb0.dll 提供的 usb_* 系列函数

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use std::os::raw::{c_int, c_uchar, c_uint, c_ushort, c_void};

// libusb0 不透明类型
pub enum usb_dev_handle {}

#[repr(C)]
pub struct usb_device_descriptor {
    pub bLength: c_uchar,
    pub bDescriptorType: c_uchar,
    pub bcdUSB: c_ushort,
    pub bDeviceClass: c_uchar,
    pub bDeviceSubClass: c_uchar,
    pub bDeviceProtocol: c_uchar,
    pub bMaxPacketSize0: c_uchar,
    pub idVendor: c_ushort,
    pub idProduct: c_ushort,
    pub bcdDevice: c_ushort,
    pub iManufacturer: c_uchar,
    pub iProduct: c_uchar,
    pub iSerialNumber: c_uchar,
    pub bNumConfigurations: c_uchar,
}

#[repr(C)]
pub struct usb_config_descriptor {
    pub bLength: c_uchar,
    pub bDescriptorType: c_uchar,
    pub wTotalLength: c_ushort,
    pub bNumInterfaces: c_uchar,
    pub bConfigurationValue: c_uchar,
    pub iConfiguration: c_uchar,
    pub bmAttributes: c_uchar,
    pub MaxPower: c_uchar,
    pub interface: *mut usb_interface,
    pub extra: *mut c_uchar,
    pub extra_length: c_int,
}

#[repr(C)]
pub struct usb_interface {
    pub altsetting: *mut usb_interface_descriptor,
    pub num_altsetting: c_int,
}

#[repr(C)]
pub struct usb_interface_descriptor {
    pub bLength: c_uchar,
    pub bDescriptorType: c_uchar,
    pub bInterfaceNumber: c_uchar,
    pub bAlternateSetting: c_uchar,
    pub bNumEndpoints: c_uchar,
    pub bInterfaceClass: c_uchar,
    pub bInterfaceSubClass: c_uchar,
    pub bInterfaceProtocol: c_uchar,
    pub iInterface: c_uchar,
    pub endpoint: *mut usb_endpoint_descriptor,
    pub extra: *mut c_uchar,
    pub extra_length: c_int,
}

#[repr(C)]
pub struct usb_endpoint_descriptor {
    pub bLength: c_uchar,
    pub bDescriptorType: c_uchar,
    pub bEndpointAddress: c_uchar,
    pub bmAttributes: c_uchar,
    pub wMaxPacketSize: c_ushort,
    pub bInterval: c_uchar,
    pub bRefresh: c_uchar,
    pub bSynchAddress: c_uchar,
    pub extra: *mut c_uchar,
    pub extra_length: c_int,
}

#[repr(C, packed)]
pub struct usb_device {
    pub next: *mut usb_device,
    pub prev: *mut usb_device,
    pub filename: [c_uchar; 512],  // LIBUSB_PATH_MAX = 512 on Windows
    pub bus: *mut usb_bus,
    pub descriptor: usb_device_descriptor,
    pub config: *mut usb_config_descriptor,
    pub dev: *mut c_void,
    pub num_children: c_uint,
    pub children: *mut *mut usb_device,
}

#[repr(C, packed)]
pub struct usb_bus {
    pub next: *mut usb_bus,
    pub prev: *mut usb_bus,
    pub dirname: [c_uchar; 512],  // LIBUSB_PATH_MAX = 512 on Windows
    pub devices: *mut usb_device,
    pub location: u32,
    pub root_dev: *mut usb_device,
}

// libusb0 函数绑定 - 链接到 libusb0.dll
#[link(name = "libusb0", kind = "dylib")]
unsafe extern "C" {
    pub fn usb_init();
    pub fn usb_find_busses() -> c_int;
    pub fn usb_find_devices() -> c_int;
    pub fn usb_get_busses() -> *mut usb_bus;
    pub fn usb_open(dev: *mut usb_device) -> *mut usb_dev_handle;
    pub fn usb_close(dev: *mut usb_dev_handle) -> c_int;
    pub fn usb_set_configuration(dev: *mut usb_dev_handle, configuration: c_int) -> c_int;
    pub fn usb_claim_interface(dev: *mut usb_dev_handle, interface: c_int) -> c_int;
    pub fn usb_release_interface(dev: *mut usb_dev_handle, interface: c_int) -> c_int;
    pub fn usb_bulk_read(dev: *mut usb_dev_handle, ep: c_int, bytes: *mut c_uchar, size: c_int, timeout: c_int) -> c_int;
    pub fn usb_bulk_write(dev: *mut usb_dev_handle, ep: c_int, bytes: *const c_uchar, size: c_int, timeout: c_int) -> c_int;
    pub fn usb_control_msg(dev: *mut usb_dev_handle, requesttype: c_int, request: c_int, value: c_int, index: c_int, bytes: *mut c_uchar, size: c_int, timeout: c_int) -> c_int;
    pub fn usb_clear_halt(dev: *mut usb_dev_handle, ep: c_uint) -> c_int;
    pub fn usb_strerror() -> *const c_uchar;
}
