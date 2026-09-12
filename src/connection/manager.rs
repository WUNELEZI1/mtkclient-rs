//! 设备连接管理器（Android 分支）
//!
//! Android 不支持 COM 口枚举和 WinUSB 驱动切换，所有 USB 访问都通过
//! Kotlin 传入的 fd 完成。这里的职责简化为：
//! 1. 等待 Kotlin 注册 USB 设备
//! 2. 打开设备 → 构造 Preloader → 执行 BROM 握手
//!
//! ```text
//! STEP 1: 等待 Kotlin 传入 fd
//! STEP 2: 从 fd 打开设备 → BROM 握手 → 关看门狗 → 获取 hw_code
//! ```

use crate::color::Colorize;
use crate::preloader::Preloader;
use crate::system::config::WorkMode;
use crate::usb::{UsbContext, UsbStage};
use log::{error, info, trace};
use std::time::Duration;

pub(crate) const RECONNECT_INTERVAL_MS: u64 = 200;
pub(crate) const RECONNECT_LOG_INTERVAL: u32 = 25;
pub(crate) const USB_REENUM_DELAY_MS: u64 = 500;
pub(crate) const QUICK_CONNECT_INTERVAL_MS: u64 = 200;
/// 连续握手失败上限：超过此次数后删除 .state 并退出程序
const MAX_CONSECUTIVE_HANDSHAKE_FAILURES: u32 = 5;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式
    Brom,
    /// Preloader 模式
    Preloader,
}

/// 统一连接管理器
pub struct ConnectionManager {
    pub(crate) mode: DeviceMode,
    pub(crate) stage: UsbStage,
    pub(crate) port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        ConnectionManager {
            mode: DeviceMode::Preloader,
            stage: UsbStage::Unknown,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
        _work_mode: WorkMode,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待 Kotlin 传入 USB 设备...");

        let mut consecutive_handshake_failures: u32 = 0;

        loop {
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            // Android 上只能打开 Kotlin 已注册的设备
            match crate::usb::context::get_android_usb_device() {
                Some(dev) => {
                    info!(
                        "[USB] 检测到已授权设备: VID=0x{:04X} PID=0x{:04X}",
                        dev.vid, dev.pid
                    );
                    match self.open_from_fd(context, dev.fd, dev.vid, dev.pid) {
                        Ok(result) => return Ok(result),
                        Err(e) => {
                            error!("[USB] 从 fd 打开设备失败: {}", e);
                            consecutive_handshake_failures += 1;
                            if consecutive_handshake_failures >= MAX_CONSECUTIVE_HANDSHAKE_FAILURES
                            {
                                crate::connection::reset_session();
                                return Err(format!(
                                    "连续 {} 次打开设备失败，请重新插拔设备后重试",
                                    consecutive_handshake_failures
                                ));
                            }
                        }
                    }
                }
                None => {
                    trace!("[USB] 等待 Kotlin 注册设备 fd...");
                }
            }

            std::thread::sleep(Duration::from_millis(RECONNECT_INTERVAL_MS));
        }
    }

    /// 从 Kotlin 传入的 fd 打开设备并完成 BROM 握手
    fn open_from_fd(
        &mut self,
        context: &UsbContext,
        fd: i32,
        vid: u16,
        pid: u16,
    ) -> Result<(Preloader, DeviceMode), String> {
        let usb_device = crate::usb::UsbDevice::from_android_fd(context, fd, vid, pid)?;

        info!(
            "[USB] 设备已打开: VID={:04X} PID={:04X} stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        let stage = usb_device.stage;
        let mut preloader = Preloader::new(Box::new(usb_device));

        // 根据 PID 决定走 BROM 还是 Preloader 握手
        if pid == 0x2000 {
            // Preloader 模式
            if !preloader
                .init_preloader()
                .map_err(|e| format!("Preloader 握手失败: {}", e))?
            {
                return Err("Preloader 握手未完成".to_string());
            }
            info!(
                "{}",
                "[USB] Preloader 握手成功（看门狗已关，HW code 已获取）"
                    .green()
                    .bold()
            );
            self.mode = DeviceMode::Preloader;
            self.stage = UsbStage::Preloader;
            return Ok((preloader, DeviceMode::Preloader));
        }

        // BROM 模式（默认）
        if !preloader
            .init()
            .map_err(|e| format!("BROM 握手失败: {}", e))?
        {
            return Err("BROM 握手未完成".to_string());
        }
        info!(
            "{}",
            "[USB] BROM 握手成功（看门狗已关，HW code 已获取）"
                .green()
                .bold()
        );
        self.mode = DeviceMode::Brom;
        self.stage = stage;
        Ok((preloader, DeviceMode::Brom))
    }
}