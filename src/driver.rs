use log::debug;

macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug {
            debug!($($arg)*);
        }
    };
}

pub fn check_driver() -> bool {
    crate::filter::is_filter_installed()
}

/// 检测 BROM 设备是否已通过 libusb 连接（VID=0E8D PID=0003）
#[allow(dead_code)]
pub fn is_brom_device_present() -> bool {
    let mut ctx: *mut libusb1_sys::libusb_context = std::ptr::null_mut();
    let init_ret = unsafe { libusb1_sys::libusb_init(&mut ctx) };
    if init_ret != 0 {
        debug_log!(true, "[DRV] libusb_init failed: {}", init_ret);
        return false;
    }
    unsafe {
        let handle = libusb1_sys::libusb_open_device_with_vid_pid(ctx, 0x0E8D, 0x0003);
        if !handle.is_null() {
            libusb1_sys::libusb_close(handle);
            debug_log!(true, "[DRV] BROM device found on USB bus");
            libusb1_sys::libusb_exit(ctx);
            return true;
        }
    }
    unsafe {
        libusb1_sys::libusb_exit(ctx);
    }
    false
}

/// 枚举 COM 口找 MediaTek BROM (VID_0E8D PID_0003)
#[allow(dead_code)]
pub fn find_mediatek_com_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for p in &ports {
        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
            && info.vid == 0x0E8D
            && info.pid == 0x0003
        {
            return Some(p.port_name.clone());
        }
    }
    None
}

/// 通过 serialport 关闭 BROM watchdog
/// 对齐 SerialPortTransport::do_handshake + WRITE32 关 WDT + 双 status 回包
#[allow(dead_code)]
pub fn disable_watchdog_brom(port_name: &str) -> Result<(), String> {
    use crate::preloader::Preloader;

    let transport = crate::preloader::SerialPortTransport::new(port_name, 115200)?;
    let mut preloader = Preloader::new(Box::new(transport));
    preloader.init()?;

    if !preloader.echo_1byte(0xD4)? {
        return Err("watchdog disable: D4 echo mismatch".into());
    }
    if !preloader.echo_4byte(0x10007000)? {
        return Err("watchdog disable: addr echo mismatch".into());
    }
    let status1 = preloader.echo_4byte_then_status(1)?;
    if status1 != 0x0001 {
        return Err(format!(
            "watchdog disable: expected status1=0x0001, got 0x{:04X}",
            status1
        ));
    }
    let status2 = preloader.echo_4byte_then_status(0x22000000)?;
    if status2 != 0x0001 {
        return Err(format!(
            "watchdog disable: expected status2=0x0001, got 0x{:04X}",
            status2
        ));
    }

    drop(preloader);
    Ok(())
}
