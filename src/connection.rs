use crate::preloader::{Preloader, SerialPortTransport};
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use colored::Colorize;
use log::{debug, info, warn};
use std::time::Duration;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（串口或 WinUSB）
    Brom,
    /// Preloader 模式（需要后续操作进入 BROM）
    Preloader,
}

/// 统一连接管理器
///
/// 连接流程：
/// ```
/// STEP 1: COM 口扫描 → 握手 → 关看门狗 → 获取 hw_code（最多重试 3 次）
///   └─ 成功 → 释放 COM 口 → 装 WinUSB → sleep → rusb 打开 → 返回
///   └─ 失败 → STEP 2
///
/// STEP 2: WinUSB 直连（降级路径，跳过握手）
///   └─ 成功 → 返回 WinUSB 设备（brom_initialized=false）
///   └─ 失败 → 报错
/// ```
pub struct ConnectionManager {
    mode: DeviceMode,
    stage: UsbStage,
    port_name: Option<String>,
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
    ///
    /// 流程：
    /// 0. 前置检测：libusb 枚举 USB 设备，如果已有 BROM 设备（WinUSB 已安装，PID=0x0003），
    ///    直接走 WinUSB 模式，跳过 COM 扫描（节省时间）
    /// 1. 无限等待串口设备出现（MediaTek USB Port）
    /// 2. 找到串口后，尝试打开并握手（最多 3 次）
    ///    ├── 成功 → 关看门狗 → 获取芯片信息 → 安装 WinUSB → 切换 USB 模式 → 返回
    ///    └── 3 次都失败 → 降级到 WinUSB 直连
    pub fn smart_init(&mut self, context: &UsbContext) -> Result<(Preloader, DeviceMode), String> {
        info!("等待设备连接 (BROM: Vol+ + Vol- + Power)");

        // === STEP 0: 前置检测 — libusb 能否直接发现 BROM 设备（WinUSB 已安装，PID=0x0003） ===
        // 如果设备已经有 WinUSB 驱动且处于 BROM 模式，就不会产生 COM 口，
        // 走 COM 扫描只会无限等待。
        if let Some((pid, dev_type)) = usb::check_mediatek_device_via_libusb() {
            info!(
                "[USB] 前置检测命中：BROM 设备 PID=0x{:04X}, type={:?}，跳过 COM 扫描",
                pid, dev_type
            );
            return self.fallback_to_winusb(context);
        }

        // === STEP 1: 无限等待串口设备出现 ===
        loop {
            match SerialPortTransport::find_brom_port_with_timeout(5000) {
                Some(port_name) => {
                    info!("[COM] 发现 BROM COM 口: {}", port_name);

                    // === STEP 2: 尝试打开串口（最多 3 次） ===
                    for attempt in 1..=3 {
                        info!("[COM] 尝试第 {}/3 次打开串口...", attempt);
                        match self.serial_handshake_and_switch(&port_name, context) {
                            Ok(preloader) => {
                                info!("{}", "COM 口前置握手成功，已切换到 WinUSB".green().bold());
                                self.mode = DeviceMode::Brom;
                                self.stage = UsbStage::Brom;
                                self.port_name = Some(port_name);
                                return Ok((preloader, DeviceMode::Brom));
                            }
                            Err(e) => {
                                warn!("[COM] 第 {}/3 次握手失败: {}", attempt, e);
                                if attempt < 3 {
                                    info!("[COM] 等待 2 秒后重试...");
                                    std::thread::sleep(Duration::from_secs(2));
                                }
                            }
                        }
                    }

                    // 3 次都失败，降级到 WinUSB
                    warn!("[COM] 连续 3 次失败，降级到 WinUSB 直连模式");
                    return self.fallback_to_winusb(context);
                }
                None => {
                    // 串口设备未出现，继续等待（无限循环）
                    debug!("[COM] 未找到 COM 口，继续等待...");
                }
            }
        }
    }

    /// WinUSB 直连（降级路径：设备已装 WinUSB 驱动 / COM 口扫描失败）
    ///
    /// 被两个入口调用：
    /// - STEP 0: 前置检测命中（设备已装 WinUSB）
    /// - STEP 2: COM 口扫描 3 次失败后降级
    ///
    /// 流程：
    /// 1. reconnect_loop 拿到 USB 设备句柄（无限等待 BROM 设备出现）
    /// 2. 构造 Preloader 并执行完整 BROM 握手（handshake → 看门狗 → HW code → target_config）
    ///    —— 这一步是关键，之前直接跳过导致设备不认识后续 DA 加载并重启
    /// 3. 返回 (Preloader, DeviceMode::Brom)
    fn fallback_to_winusb(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("[USB] 尝试 WinUSB 直连...");
        info!("[USB] 等待 BROM 设备出现 (PID=0x0003, 无限等待)...");

        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "[USB] WinUSB 设备已打开: VID={:04x}, PID={:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        // 构造 Preloader 并执行完整 BROM 握手
        // —— 这一步是修复：之前直接 brom_initialized = false 跳过 init，
        //    后续 Kamakiri2 步骤会让设备"不认识"而重启
        let mut preloader = Preloader::new(Box::new(usb_device));
        if !preloader
            .init()
            .map_err(|e| format!("WinUSB BROM 握手失败: {}", e))?
        {
            return Err("WinUSB BROM 握手未完成".to_string());
        }
        info!(
            "{}",
            "[USB] BROM 握手成功（看门狗已关，HW code 已获取）"
                .green()
                .bold()
        );

        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Brom;
        Ok((preloader, DeviceMode::Brom))
    }

    /// COM 口前置握手 → 释放 → WinUSB 接管
    ///
    /// 流程：
    /// 1. 打开 COM 口
    /// 2. 握手 + init()（关看门狗 + 获取 hw_code）
    /// 3. 释放 COM 口
    /// 4. wdi-rs 切换到 WinUSB（卸载 usbser.sys，安装 WinUSB）
    /// 5. 等待设备重枚举
    /// 6. libusb1-sys 打开设备
    /// 7. 返回 WinUSB Preloader（chip 已从 COM 口获取）
    fn serial_handshake_and_switch(
        &self,
        port_name: &str,
        context: &UsbContext,
    ) -> Result<Preloader, String> {
        // 1. 打开串口
        let transport = SerialPortTransport::new(port_name, 115200)?;
        let mut serial_preloader = Preloader::new(Box::new(transport));

        // 2. 完整 init：握手 + 关看门狗 + 获取 hw_code + 设置 chip
        if !serial_preloader
            .init()
            .map_err(|e| format!("串口 init 失败: {}", e))?
        {
            return Err("串口握手失败".to_string());
        }

        let chip = serial_preloader.chip;
        info!("COM 口 init 成功，chip={:?}", chip.map(|c| c.hw_code));

        // 3. 释放 COM 口
        drop(serial_preloader);
        info!("COM 口已释放");

        // 4. wdi-rs 切换到 WinUSB（卸载 usbser.sys，安装 WinUSB）
        crate::driver::switch_to_winusb().map_err(|e| format!("切换 WinUSB 驱动失败: {}", e))?;

        // 5. 等待设备重枚举（驱动切换后设备会重新枚举）
        std::thread::sleep(Duration::from_secs(3));

        // 6. libusb1-sys 打开设备（轮询 10 秒）
        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;
        info!(
            "WinUSB 打开成功: VID={:04X} PID={:04X}",
            usb_device.vid, usb_device.pid
        );

        // 7. 构造 Preloader（chip 已从 COM 口获取，brom_initialized=true）
        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.chip = chip;
        preloader.brom_initialized = true;

        Ok(preloader)
    }

    /// WinUSB 重连循环
    /// 无限等待直到设备出现（用户手动按组合键进入 BROM 模式可能需要几十秒到几分钟）
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        target_stage: UsbStage,
    ) -> Result<usb::UsbDevice, String> {
        const INTERVAL_MS: u64 = 200;
        let mut retry = 0;

        info!(
            "[RECONNECT] scanning for stage={:?} (infinite wait)...",
            target_stage
        );

        let pids = match target_stage {
            UsbStage::Brom => vec![0x0003u16],
            UsbStage::Preloader => vec![0x2000u16],
            UsbStage::Da => vec![0x2001u16],
            UsbStage::Unknown => vec![0x0003u16, 0x2000u16, 0x2001u16],
        };

        loop {
            retry += 1;

            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        info!(
                            "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                            retry, device.stage, device.pid
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        debug!(
                            "[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}",
                            pid, e
                        );
                    }
                }
            }

            if retry % 25 == 1 {
                debug!(
                    "[RECONNECT] retry {} (scanning {} PIDs)...",
                    retry,
                    pids.len()
                );
            }
            std::thread::sleep(Duration::from_millis(INTERVAL_MS));
        }
    }

    /// DA 加载后重连
    #[allow(dead_code)] // 预留：DA 加载后设备重枚举流程
    pub fn reconnect_after_da(&self, context: &UsbContext) -> Result<usb::UsbDevice, String> {
        info!("[DA] DA 加载完成，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));

        info!("[DA] 尝试 BROM PID (0x0003)...");
        if let Ok(device) = self.try_quick_connect(context, 0x0E8D, 0x0003, 5) {
            info!(
                "[DA] 连接成功: VID=0x{:04X} PID=0x{:04X}",
                device.vid, device.pid
            );
            return Ok(device);
        }

        info!("[DA] BROM PID 失败，扫描所有已知 PID...");
        self.reconnect_loop(context, UsbStage::Unknown)
    }

    /// Kamakiri exploit 后重连
    #[allow(dead_code)] // 预留：Kamakiri2 exploit 后设备重枚举流程
    pub fn reconnect_after_kamakiri(&self, context: &UsbContext) -> Result<usb::UsbDevice, String> {
        info!("[KAMAKIRI] payload 已发送，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// USB reset 后重连
    #[allow(dead_code)] // 预留：USB reset 后设备重枚举流程
    pub fn reconnect_after_usb_reset(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[USB] USB reset detected, waiting for re-enumeration...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// 快速连接尝试
    #[allow(dead_code)] // 预留：DA 后快速重连场景（被 reconnect_after_da 内部调用）
    fn try_quick_connect(
        &self,
        context: &UsbContext,
        vid: u16,
        pid: u16,
        retries: usize,
    ) -> Result<usb::UsbDevice, String> {
        for _ in 1..=retries {
            match usb::UsbDevice::open_by_vid_pid(context, vid, pid) {
                Ok(device) => return Ok(device),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        Err(format!("快速连接失败 ({} 次重试)", retries))
    }

    /// 获取当前连接模式
    #[allow(dead_code)] // 预留：查询当前连接状态（串口/USB）
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取当前 USB 阶段
    #[allow(dead_code)] // 预留：查询设备当前所处阶段（BROM/Preloader）
    pub fn stage(&self) -> &UsbStage {
        &self.stage
    }

    /// 获取串口名称
    #[allow(dead_code)] // 预留：调试/日志输出当前使用的串口名
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }

    /// DA 会话复用入口：直接连接到已处于 DA 模式的设备（PID=0x2000）
    ///
    /// 用于以下场景：
    /// - 设备已加载 DA，PID 切换为 0x2000（Preloader 模式）
    /// - 上次 .state 文件中记录了 da_loaded=true
    /// - 用户希望跳过 BROM→DA 流程，直接使用现有 DA 会话
    ///
    /// 流程：
    /// 1. 等待 USB 设备出现（PID=0x2000）
    /// 2. 构造 Preloader（DA 模式）
    /// 3. 不做 BROM 握手（设备已加载 DA，无须握手）
    /// 4. 返回 (Preloader, DeviceMode::Brom) — Preloader 内部 is_preloader_mode=true
    pub fn connect_to_da_mode(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!("[DA_SESSION] 直接连接 DA 模式设备 (PID=0x2000)...");
        info!("[DA_SESSION] 等待 Preloader 设备出现 (PID=0x2000)...");

        let usb_device = self.reconnect_loop(context, UsbStage::Preloader)?;

        info!(
            "[DA_SESSION] DA 设备已连接: VID={:04X}, PID={:04X}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        // 构造 Preloader（DA 模式，不做 BROM 握手）
        let mut preloader = Preloader::new(Box::new(usb_device));
        preloader.is_preloader_mode = true; // 标记为 DA/Preloader 模式，跳过 BROM 流程
        preloader.brom_initialized = true; // 标记为已初始化（DA 模式不需要 BROM 握手）

        // 从 .state 恢复 chip 配置（如果 .state 中有 hw_code）
        #[allow(clippy::collapsible_if)]
        if let Some(state) = crate::session::SessionState::load() {
            if let Some(chip) = crate::config::CHIP_CONFIGS
                .iter()
                .find(|c| c.hw_code == state.hw_code)
            {
                preloader.chip = Some(*chip);
                info!(
                    "[DA_SESSION] 从 .state 恢复 chip 配置: HW code=0x{:04X}",
                    state.hw_code
                );
            }
        }

        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Preloader;

        info!("{}", "[DA_SESSION] DA 会话复用成功".green().bold());
        Ok((preloader, DeviceMode::Brom))
    }
}
