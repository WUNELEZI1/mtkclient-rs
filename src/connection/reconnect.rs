use crate::preloader::Preloader;
use crate::usb;
use crate::usb::{USB上下文, USB阶段};
use colored::Colorize;
use log::{info, trace, warn};
use std::time::Duration;

use super::manager::{ConnectionManager, DeviceMode};

use crate::preloader::SerialPortTransport;

impl ConnectionManager {
    /// WinUSB 重连循环
    /// 无限等待直到设备出现（用户手动按组合键进入 BROM 模式可能需要几十秒到几分钟）
    pub fn reconnect_loop(
        &self,
        context: &USB上下文,
        target_stage: USB阶段,
    ) -> Result<usb::USB设备, String> {
        let mut retry = 0;
        let mut context_refreshed = false;

        info!(
            "[RECONNECT] scanning for stage={:?} (infinite wait)...",
            target_stage
        );

        let pids = match target_stage {
            USB阶段::Brom => vec![0x0003u16],
            USB阶段::Preloader => vec![0x2000u16],
            USB阶段::Da => vec![0x2001u16],
            USB阶段::未知 => vec![0x0003u16, 0x2000u16, 0x2001u16],
        };

        loop {
            retry += 1;

            // Ctrl+C 检查：用户取消时退出等待
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            for &pid in &pids {
                match usb::USB设备::按VID_PID打开(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!(
                            "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                            retry, device.阶段, device.pid
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        trace!(
                            "[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}",
                            pid, e
                        );
                    }
                }
            }

            // 驱动切换后旧的 libusb context 可能无法枚举新设备
            // 每 50 次重试（约 10 秒）尝试用新 context 打开
            if retry % 50 == 0 && !context_refreshed {
                trace!("[RECONNECT] 尝试刷新 libusb context...");
                if let Ok(new_ctx) = USB上下文::新建() {
                    for &pid in &pids {
                        if let Ok(device) = usb::USB设备::按VID_PID打开(&new_ctx, 0x0E8D, pid) {
                            info!("[RECONNECT] 使用新 context 成功连接 (attempt {})", retry);
                            return Ok(device);
                        }
                    }
                }
                context_refreshed = true;
            }

            if retry % super::manager::RECONNECT_LOG_INTERVAL == 1 {
                trace!(
                    "[RECONNECT] retry {} (scanning {} PIDs)...",
                    retry,
                    pids.len()
                );
            }
            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    /// DA 加载后重连
    #[allow(dead_code)] // 预留：DA 加载后设备重枚举流程
    pub fn reconnect_after_da(&self, context: &USB上下文) -> Result<usb::USB设备, String> {
        info!("[DA] DA 加载完成，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(super::manager::USB_REENUM_DELAY_MS));

        info!("[DA] 尝试 BROM PID (0x0003)...");
        if let Ok(device) = self.try_quick_connect(context, 0x0E8D, 0x0003, 5) {
            info!(
                "[DA] 连接成功: VID=0x{:04X} PID=0x{:04X}",
                device.vid, device.pid
            );
            return Ok(device);
        }

        info!("[DA] BROM PID 失败，扫描所有已知 PID...");
        self.reconnect_loop(context, USB阶段::未知)
    }

    /// Kamakiri exploit 后重连
    #[allow(dead_code)] // 预留：Kamakiri2 exploit 后设备重枚举流程
    pub fn reconnect_after_kamakiri(
        &self, context: &USB上下文
    ) -> Result<usb::USB设备, String> {
        info!("[KAMAKIRI] payload 已发送，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(super::manager::USB_REENUM_DELAY_MS));
        self.reconnect_loop(context, USB阶段::Brom)
    }

    /// USB reset 后重连
    #[allow(dead_code)] // 预留：USB reset 后设备重枚举流程
    pub fn reconnect_after_usb_reset(
        &self, context: &USB上下文
    ) -> Result<usb::USB设备, String> {
        info!("[USB] USB reset detected, waiting for re-enumeration...");
        std::thread::sleep(Duration::from_millis(super::manager::USB_REENUM_DELAY_MS));
        self.reconnect_loop(context, USB阶段::Brom)
    }

    /// 快速连接尝试
    #[allow(dead_code)] // 预留：DA 后快速重连场景（被 reconnect_after_da 内部调用）
    fn try_quick_connect(
        &self,
        context: &USB上下文,
        vid: u16,
        pid: u16,
        retries: usize,
    ) -> Result<usb::USB设备, String> {
        for _ in 1..=retries {
            match usb::USB设备::按VID_PID打开(context, vid, pid) {
                Ok(device) => return Ok(device),
                Err(_) => std::thread::sleep(Duration::from_millis(
                    super::manager::QUICK_CONNECT_INTERVAL_MS,
                )),
            }
        }
        Err(format!("快速连接失败 ({} 次重试)", retries))
    }

    /// DA 会话复用入口：直接连接到已处于 DA 模式的设备
    /// MTK 设备加载 DA 后通常仍使用 PID=0x0003（与 BROM 相同），
    /// 所以不能只等待 PID=0x2000，应该直接打开当前已枚举的设备
    pub fn connect_to_da_mode(
        &mut self,
        context: &USB上下文,
    ) -> Result<(Preloader, DeviceMode), String> {
        // 从 .state 获取上次使用的 VID/PID
        let state = match crate::connection::SessionState::load() {
            Some(s) => s,
            None => return Err("[DA_SESSION] .state 不存在".to_string()),
        };

        info!(
            "[DA_SESSION] 尝试连接 DA 设备 (VID={:04X}, PID={:04X})...",
            state.usb_vid, state.usb_pid
        );

        // 直接用 .state 中的 VID/PID 打开设备（不等待特定 PID）
        let usb_device =
            match usb::USB设备::按VID_PID打开(context, state.usb_vid, state.usb_pid) {
                Ok(dev) => dev,
                Err(e) => {
                    // 如果指定 PID 打开失败，尝试扫描所有已知 MTK PID
                    warn!(
                        "[DA_SESSION] PID={:04X} 打开失败 ({})，扫描所有 MTK PID...",
                        state.usb_pid, e
                    );
                    let usb_device = self.reconnect_loop(context, USB阶段::未知)?;
                    info!(
                        "[DA_SESSION] 扫描连接成功: VID={:04X}, PID={:04X}",
                        usb_device.vid, usb_device.pid
                    );
                    usb_device
                }
            };

        info!(
            "[DA_SESSION] DA 设备已连接: VID={:04X}, PID={:04X}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.阶段
        );

        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.is_preloader_mode = true;
        preloader.brom_initialized = true;

        // 从 .state 恢复 chip 配置（如果 .state 中有 hw_code）
        #[allow(clippy::collapsible_if)]
        if let Some(chip) = crate::system::config::CHIP_CONFIGS
            .iter()
            .find(|c| c.hw_code == state.hw_code)
        {
            preloader.chip = Some(*chip);
            info!(
                "[DA_SESSION] 从 .state 恢复 chip 配置: HW code=0x{:04X}",
                state.hw_code
            );
        }

        self.mode = DeviceMode::Brom;
        self.stage = USB阶段::Preloader;

        info!("{}", "[DA_SESSION] DA 会话复用成功".green().bold());
        Ok((preloader, DeviceMode::Brom))
    }

    /// 获取当前连接模式
    #[allow(dead_code)] // 预留：查询当前连接状态（串口/USB）
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取当前 USB 阶段
    #[allow(dead_code)] // 预留：查询设备当前所处阶段（BROM/Preloader）
    pub fn stage(&self) -> &USB阶段 {
        &self.stage
    }

    /// 获取串口名称
    #[allow(dead_code)] // 预留：调试/日志输出当前使用的串口名
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// Preloader 模式初始化：等待 Preloader VCOM (PID=0x2000)，握手后直接返回
    /// 如果找不到 Preloader 设备但检测到 BROM 设备，自动 fallback 到 BROM 模式
    pub(crate) fn smart_init_preloader(
        &mut self,
        context: &USB上下文,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("等待 Preloader VCOM 设备连接 (PID=0x2000)，无需按任何按键...");

        // 0. 尝试复用已有 DA 会话（设备已在 DA 模式，跳过 BROM 握手）
        if let Some(state) = crate::connection::session::SessionState::load() {
            if state.da_loaded && (state.usb_pid == 0x2000 || state.usb_pid == 0x2001) {
                if let Ok(ports) = serialport::available_ports() {
                    for p in &ports {
                        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                            && info.vid == 0x0E8D
                            && (info.pid == 0x2000 || info.pid == 0x2001)
                        {
                            info!(
                                "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})，尝试 DA 会话复用",
                                p.port_name, info.vid, info.pid
                            );
                            match SerialPortTransport::new(&p.port_name, 115200) {
                                Ok(transport) => {
                                    let mut preloader = Preloader::new(Box::new(transport));
                                    preloader.is_preloader_mode = true;
                                    preloader.brom_initialized = true;
                                    // 从 .state 恢复 chip 配置（供后续 save_session_state 使用）
                                    if let Some(chip) = crate::system::config::CHIP_CONFIGS
                                        .iter()
                                        .find(|c| c.hw_code == state.hw_code)
                                    {
                                        preloader.chip = Some(*chip);
                                    }
                                    self.mode = DeviceMode::Preloader;
                                    self.stage = USB阶段::Preloader;
                                    self.port_name = Some(p.port_name.clone());
                                    info!("[PRELOADER] DA 会话复用：跳过 BROM 握手，直接返回");
                                    return Ok((preloader, DeviceMode::Preloader));
                                }
                                Err(e) => {
                                    warn!("[PRELOADER] 复用会话时打开串口失败: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut retry_count = 0u32;
        const PRELOADER_MAX_RETRY: u32 = 50; // 约 10 秒 (50 * 200ms)

        loop {
            retry_count += 1;

            // Ctrl+C 检查：用户取消时退出等待
            if crate::cancel::requested() {
                return Err("用户取消等待".to_string());
            }

            // 1. 枚举 COM 口，找 PID=0x2000 的 Preloader VCOM
            if let Ok(ports) = serialport::available_ports() {
                for p in &ports {
                    if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                        && info.vid == 0x0E8D
                        && info.pid == 0x2000
                    {
                        info!(
                            "[PRELOADER] 发现 Preloader COM 口: {} (VID={:04X} PID={:04X})",
                            p.port_name, info.vid, info.pid
                        );
                        match self.preloader_serial_handshake(&p.port_name) {
                            Ok(preloader) => {
                                self.mode = DeviceMode::Preloader;
                                self.stage = USB阶段::Preloader;
                                self.port_name = Some(p.port_name.clone());
                                return Ok((preloader, DeviceMode::Preloader));
                            }
                            Err(e) => {
                                warn!("[PRELOADER] {} 握手失败: {}", p.port_name, e);
                            }
                        }
                    }
                }
            }

            // 2. 串口没找到，尝试 WinUSB（PID=0x2000）
            if let Ok(usb_device) = usb::USB设备::按VID_PID打开(context, 0x0E8D, 0x2000) {
                info!(
                    "[PRELOADER] WinUSB 设备已连接: VID={:04X} PID={:04X}",
                    usb_device.vid, usb_device.pid
                );
                let mut preloader = Preloader::new(Box::new(usb_device));
                match preloader.init_preloader() {
                    Ok(true) => {
                        self.mode = DeviceMode::Preloader;
                        self.stage = USB阶段::Preloader;
                        return Ok((preloader, DeviceMode::Preloader));
                    }
                    Ok(false) => {
                        warn!("[PRELOADER] init 未成功，继续等待...");
                    }
                    Err(e) => {
                        warn!("[PRELOADER] init 失败: {}，继续等待...", e);
                    }
                }
            }

            // 3. 尝试一定次数后仍未找到 Preloader 设备，检测 BROM 设备并 fallback
            if retry_count >= PRELOADER_MAX_RETRY {
                warn!(
                    "[PRELOADER] 等待 {} 次 (约 {} 秒) 未找到 Preloader 设备，尝试检测 BROM 设备...",
                    retry_count,
                    (retry_count * 200) / 1000
                );

                // 检测 BROM COM 口
                if let Ok(ports) = serialport::available_ports() {
                    for p in &ports {
                        if let serialport::SerialPortType::UsbPort(ref info) = p.port_type
                            && info.vid == 0x0E8D
                            && info.pid == 0x0003
                        {
                            warn!(
                                "[PRELOADER] 检测到 BROM COM 口: {}，自动切换到 BROM 模式",
                                p.port_name
                            );
                            return self.smart_init(context, crate::system::config::工作模式::Brom);
                        }
                    }
                }

                // 检测 BROM WinUSB 设备
                if let Ok(_usb_device) = usb::USB设备::按VID_PID打开(context, 0x0E8D, 0x0003) {
                    warn!("[PRELOADER] 检测到 BROM WinUSB 设备 (PID=0x0003)，自动切换到 BROM 模式");
                    return self.smart_init(context, crate::system::config::工作模式::Brom);
                }

                // 重置计数器继续等待（给用户更多时间）
                retry_count = 0;
                warn!("[PRELOADER] 未检测到 BROM 设备，继续等待 Preloader 设备...");
            }

            std::thread::sleep(Duration::from_millis(super::manager::RECONNECT_INTERVAL_MS));
        }
    }

    /// Preloader 串口握手：打开 COM 口 → init_preloader
    fn preloader_serial_handshake(&self, port_name: &str) -> Result<Preloader, String> {
        let transport = SerialPortTransport::new(port_name, 115200)?;
        let mut preloader = Preloader::new(Box::new(transport));
        if !preloader
            .init_preloader()
            .map_err(|e| format!("串口 init_preloader 失败: {}", e))?
        {
            return Err("串口 Preloader 握手失败".to_string());
        }
        info!(
            "{}",
            format!("[PRELOADER] {} 握手成功", port_name).green().bold()
        );
        Ok(preloader)
    }
}
