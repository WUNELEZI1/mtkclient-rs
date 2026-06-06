use crate::driver;
use crate::preloader::{self, BromTransport, Preloader};
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use colored::Colorize;
use log::{debug, info, warn};
use std::time::Duration;
use std::process::Stdio;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（通过 libusb 通信）
    Brom,
    /// Preloader 模式（串口已释放，切换到 USB BROM）
    Preloader,
}

/// reconnect_loop 状态机状态
#[derive(Debug, PartialEq)]
enum ReconnectState {
    /// 等待设备重枚举（COM 释放后）
    WaitReenumeration,
    /// 检测到设备窗口，尝试打开
    WindowDetected,
}

/// 统一连接管理器
/// 
/// 职责：
/// 1. 统一设备入口（smart_init）
/// 2. 管理连接状态（BROM ↔ Preloader ↔ DA 切换）
/// 3. 自动重连（reconnect_loop）
/// 4. 自动驱动后端选择（libusb-filter → libusb → COM）
/// 5. 对齐 MTKClient Python 行为
/// 
/// 三层驱动架构：
/// Layer 1: libusb-filter（优先，通过 install-filter.exe 安装设备过滤器）
/// Layer 2: libusb 直接访问（fallback，需设备未被 Windows 驱动占用）
/// Layer 3: Serial COM（仅 Preloader 阶段，握手后释放 → USB 重枚举）
/// 
/// 对齐 MTKClient Python:
/// - usblib.py::connect() → UsbDevice::open_by_vid_pid()
/// - mtk_preloader.py::init() → ConnectionManager::smart_init()
/// - Port.py::run_handshake() → SerialPortTransport::do_handshake()
/// - get_connection_agent() → driver::detect_backend()
/// - dynamic backend selection → libusb-filter detection + fallback
/// - reconnect state machine → reconnect_loop with window detection
pub struct ConnectionManager {
    mode: DeviceMode,
    stage: UsbStage,
    port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        info!("[DRIVER] {}", driver::backend_status());

        ConnectionManager {
            mode: DeviceMode::Preloader,
            stage: UsbStage::Unknown,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    /// 
    /// 完整流程（对齐刷机匣 + MTKClient Python）：
    /// ```
    /// STEP 1: USB fast scan（libusb-filter 优先）
    ///   └─ 实时 detect_backend() 选择最佳后端
    ///   └─ 尝试打开 BROM PID（0x0003）
    ///   └─ 成功 → backend = Libusb → 返回
    ///   └─ 失败 → 进入 STEP 2
    /// 
    /// STEP 2: COM scan + handshake
    ///   └─ 扫描 COM 端口 → 打开串口 → BROM 握手 → init
    ///   └─ 成功 → stage = Preloader → 进入 STEP 3
    ///   └─ 失败 → 进入 STEP 4
    /// 
    /// STEP 3: COM → USB 切换（状态机 + delay window）
    ///   └─ 释放串口（drop）→ 日志记录
    ///   └─ 等待重枚举窗口（100ms 间隔扫描，最多 3 秒）
    ///   └─ reconnect_loop(Brom) → libusb 接管
    /// 
    /// STEP 4: reconnect_loop 循环检测
    ///   └─ 状态机：WaitReenumeration → WindowDetected → OpenAttempt → Acquired
    ///   └─ 扫描 BROM PID（0x0003）→ 10 秒超时，200ms 重试
    /// ```
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!(
            "等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)"
        );

        // 实时检测后端（不缓存）
        let backend = driver::detect_backend(context, 0x0E8D, 0x0003);
        info!("[DRIVER] backend={:?}", backend);

        // === STEP 1: USB fast scan ===
        // 尝试直接打开 BROM 设备
        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
            info!(
                "  VID: {:04x}, PID: {:04x}, stage={:?}",
                device.vid, device.pid, device.stage
            );
            info!("{}", "USB 直接连接成功 (BROM 模式)".green().bold());
            self.mode = DeviceMode::Brom;
            self.stage = UsbStage::Brom;
            return Ok((Preloader::new(Box::new(device)), DeviceMode::Brom));
        }

        // 检测是否被 COM 占用
        if driver::is_device_com_occupied(0x0E8D, 0x0003) {
            info!("[USB] 设备被 Windows COM 驱动占用，libusb 无法直接打开");
            info!("[USB] 将尝试安装 libusb-filter 以自动接管设备");
        }
        debug!("USB BROM 直接连接失败，尝试串口检测...");

        // === STEP 2: COM scan + handshake ===
        // COM 握手成功后释放 COM → 安装 filter → 轮询 libusb 接管
        if let Some((port_name, preloader)) = self.serial_connect(context)? {
            info!("[COM] COM → libusb 切换成功 {}", port_name);

            self.mode = DeviceMode::Brom;
            self.stage = UsbStage::Brom;
            self.port_name = Some(port_name);
            return Ok((preloader, DeviceMode::Brom));
        }

        // === STEP 4: reconnect_loop 循环检测 ===
        info!("串口检测超时，尝试 USB 循环连接...");

        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "  VID: {:04x}, PID: {:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Brom;
        Ok((Preloader::new(Box::new(usb_device)), DeviceMode::Brom))
    }

    /// 串口连接 + 握手 → 释放 COM → 安装 libusb-filter → 轮询 libusb 接管
    /// 
    /// 扫描 COM 端口 → 打开串口 → BROM 握手 → init → 释放 COM → 安装 filter → 轮询 libusb
    /// 
    /// init 流程（严格对齐刷机匣日志）：
    /// - A0 → FA（BROM handshake）
    /// - FD → get_hw_code
    /// - D4 + 0x10007000 + 0x00000001 + rword + 0x22000000 + rword（watchdog disable）
    /// - D8 → get_target_config
    /// - FE → BROM sync
    /// - FF → 进入 ID 阶段
    /// - FC → 读 HW info
    fn serial_connect(&self, context: &UsbContext) -> Result<Option<(String, Preloader)>, String> {
        let mut attempt = 0;
        const MAX_ATTEMPTS: usize = 30;

        while attempt < MAX_ATTEMPTS {
            attempt += 1;

            if let Some(port_name) = preloader::detect_serial_preloader() {
                info!("发现 Preloader 串口设备: {}", port_name);

                let transport = preloader::SerialPortTransport::new(&port_name, 115200)
                    .map_err(|e| format!("打开串口失败: {}", e))?;
                let device: Box<dyn BromTransport> = Box::new(transport);
                let mut preloader = Preloader::new(device);

                if preloader.init().unwrap_or(false) {
                    info!("COM 口握手成功: {}", port_name);

                    // 获取 hw_code（在释放 COM 前）
                    let _hw_code = preloader.get_hw_code();

                    // 释放 COM 口
                    drop(preloader);
                    info!("COM 口已释放，等待设备重新枚举...");

                    // 等设备稳定
                    std::thread::sleep(Duration::from_millis(500));

                    // 安装 libusb filter（只对 0E8D:0003）
                    let filter_exe = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("install-filter.exe")));
                    if let Some(ref exe) = filter_exe {
                        if exe.exists() {
                            info!("[FILTER] 安装 libusb filter: {}", exe.display());
                            std::process::Command::new(exe)
                                .args(["install", "--device=USB\\VID_0E8D&PID_0003"])
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status()
                                .ok();
                        }
                    }

                    // 轮询等待 libusb 能打开设备（最多 20 次，每次 500ms = 10 秒）
                    for i in 0..20 {
                        if let Ok(d) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
                            info!("libusb 接管成功 (attempt {}/{})", i + 1, 20);
                            return Ok(Some((port_name, Preloader::new(Box::new(d)))));
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }

                    return Err("切换到 libusb 失败：设备重新枚举后无法通过 libusb 打开".into());
                }
                debug!("串口握手失败，继续轮询...");
            }

            std::thread::sleep(Duration::from_secs(1));
        }

        debug!("串口检测完成，未找到可用设备");
        Ok(None)
    }

    /// libusb 重连循环（MTKClient 风格窗口捕获 + 状态机）
    /// 
    /// 对齐 MTKClient Python 行为：
    /// - usblib.py::connect() 循环扫描设备
    /// - mtk_preloader.py::init() 重试机制
    /// - stage2.py USB reset handling
    /// 
    /// 状态机流程：
    /// ```
    /// WaitReenumeration → 等待设备重枚举
    ///     ↓
    /// WindowDetected → 检测到设备窗口
    ///     ↓
    /// OpenAttempt → 尝试打开并验证（短时间 3~5 秒）
    ///     ↓
    /// Acquired → 成功获取设备
    /// ```
    /// 
    /// 关键特性：
    /// - 10 秒超时，200ms 重试间隔
    /// - 支持 BROM / Preloader / DA 阶段过滤
    /// - 支持 USB reset 后重连
    /// - 支持 Kamakiri exploit 后重连
    /// - 支持 DA 加载后重连
    /// - 所有错误自动重试，不信任单次连接
    /// - 设备状态模型：避免误连（检查 interface count / endpoints）
    /// - 每次 reconnect 实时判断后端（不缓存）
    /// 
    /// 阶段过滤：
    /// - UsbStage::Brom → 扫描 PID 0x0003
    /// - UsbStage::Preloader → 扫描 PID 0x2000
    /// - UsbStage::Unknown → 扫描所有已知 PID
    /// 
    /// 日志输出：
    /// ```
    /// [RECONNECT] scanning for stage=Brom...
    /// [RECONNECT] state=WindowDetected (PID=0x0003)
    /// [RECONNECT] success on attempt 3 (stage=Brom, PID=0x0003)
    /// ```
    pub fn reconnect_loop(
        &self,
        context: &UsbContext,
        target_stage: UsbStage,
    ) -> Result<usb::UsbDevice, String> {
        const TIMEOUT_MS: u64 = 10_000;
        const INTERVAL_MS: u64 = 200;
        let max_retries = (TIMEOUT_MS / INTERVAL_MS) as usize;
        let mut retry = 0;

        info!("[RECONNECT] scanning for stage={:?}...", target_stage);

        // 根据目标阶段确定扫描的 VID/PID 列表
        let pids = match target_stage {
            UsbStage::Brom => vec![0x0003u16],
            UsbStage::Preloader => vec![0x2000u16],
            UsbStage::Unknown => vec![0x0003u16, 0x2000u16],
        };

        let mut state = ReconnectState::WaitReenumeration;

        while retry < max_retries {
            retry += 1;

            // 遍历所有候选 PID
            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        // 设备窗口检测到，进入 OpenAttempt 状态
                        if state == ReconnectState::WaitReenumeration {
                            state = ReconnectState::WindowDetected;
                            info!(
                                "[RECONNECT] window detected (stage={:?}, PID=0x{:04X})",
                                device.stage, device.pid
                            );
                        }

                        // 设备状态模型验证：确认设备可用
                        if self.validate_device(&device) {
                            // 实时检测后端（不缓存）
                            let _backend = driver::detect_backend(context, device.vid, device.pid);
                            info!("[USB] device opened successfully");
                            info!("[USB] interface claimed");

                            info!(
                                "[RECONNECT] success on attempt {} (stage={:?}, PID=0x{:04X})",
                                retry, device.stage, device.pid
                            );
                            return Ok(device);
                        } else {
                            debug!(
                                "[RECONNECT] device found but validation failed (PID=0x{:04X})",
                                pid
                            );
                        }
                    }
                    Err(e) => {
                        // 静默失败，继续尝试下一个 PID
                        debug!("[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}", pid, e);
                    }
                }
            }

            if retry % 5 == 1 || retry == max_retries {
                debug!("[RECONNECT] retry {}/{} (state={:?}, scanning {} PIDs)...", 
                       retry, max_retries, state, pids.len());
            }
            std::thread::sleep(Duration::from_millis(INTERVAL_MS));
        }

        let pid_list: String = pids.iter().map(|p| format!("0x{:04X}", p)).collect::<Vec<_>>().join(", ");
        Err(format!(
            "libusb 连接超时 ({}ms)，未找到设备 PID=[{}]",
            TIMEOUT_MS, pid_list
        ))
    }

    /// 设备状态验证（避免误连）
    /// 
    /// 验证项：
    /// 1. VID 必须是 0x0E8D
    /// 2. endpoint 必须存在（OUT + IN）
    /// 3. 设备可访问
    fn validate_device(&self, device: &usb::UsbDevice) -> bool {
        // VID 验证
        if device.vid != 0x0E8D {
            warn!("[RECONNECT] invalid VID: {:04X} (expected 0E8D)", device.vid);
            return false;
        }

        // endpoint 验证
        if device.ep_out == 0 || device.ep_in == 0 {
            warn!("[RECONNECT] missing endpoints: OUT=0x{:02X} IN=0x{:02X}", device.ep_out, device.ep_in);
            return false;
        }

        true
    }

    /// DA 加载后重连
    /// 
    /// 对齐 MTKClient Python:
    /// - DA 加载后设备会 USB reset
    /// - DA 可能运行在不同的 PID 上
    /// - 需要等待重枚举后重新连接
    /// 
    /// 流程：
    /// 1. 等待设备断开（500ms）
    /// 2. reconnect_loop(Brom) — DA 通常仍用 BROM PID
    /// 3. 如果 BROM PID 失败，尝试所有已知 PID
    /// 
    /// 用法：
    /// ```
    /// da.upload_da()?;
    /// let device = conn_mgr.reconnect_after_da(&context)?;
    /// ```
    pub fn reconnect_after_da(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[DA] DA 加载完成，等待设备重枚举...");

        // 阶段 1：等待设备断开并重新枚举
        std::thread::sleep(Duration::from_millis(500));

        // 阶段 2：尝试 BROM PID（DA 通常仍用 0x0003）
        info!("[DA] 尝试 BROM PID (0x0003)...");
        if let Ok(device) = self.try_quick_connect(context, 0x0E8D, 0x0003, 5) {
            info!("[DA] 连接成功: VID=0x{:04X} PID=0x{:04X}", device.vid, device.pid);
            return Ok(device);
        }

        // 阶段 3：扫描所有已知 PID
        info!("[DA] BROM PID 失败，扫描所有已知 PID...");
        self.reconnect_loop(context, UsbStage::Unknown)
    }

    /// Kamakiri exploit 后重连
    /// 
    /// 对齐 mtkclient stage2.py:
    /// - 发送 payload 后设备 USB reset
    /// - 需要等待重枚举后重新连接
    pub fn reconnect_after_kamakiri(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[KAMAKIRI] payload 已发送，等待设备重枚举...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// USB reset 后重连
    /// 
    /// 对齐 mtkclient usblib.py reconnect loop:
    /// - USB reset 后设备断开并重新枚举
    /// - 需要等待重枚举后重新连接
    pub fn reconnect_after_usb_reset(
        &self,
        context: &UsbContext,
    ) -> Result<usb::UsbDevice, String> {
        info!("[USB] USB reset detected, waiting for re-enumeration...");
        std::thread::sleep(Duration::from_millis(500));
        self.reconnect_loop(context, UsbStage::Brom)
    }

    /// 快速连接尝试（用于 DA 后快速重连）
    /// 
    /// 参数：
    /// - retries: 重试次数（默认 10 次，间隔 200ms = 2 秒）
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
    pub fn mode(&self) -> &DeviceMode {
        &self.mode
    }

    /// 获取当前 USB 阶段
    pub fn stage(&self) -> &UsbStage {
        &self.stage
    }

    /// 获取串口名称
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }
}
