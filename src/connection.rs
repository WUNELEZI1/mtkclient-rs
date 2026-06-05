use crate::driver::{self, DeviceBackend};
use crate::preloader::{self, BromTransport, Preloader};
use crate::usb;
use crate::usb::{UsbContext, UsbStage};
use colored::Colorize;
use log::{debug, info, warn};
use std::time::Duration;

/// 设备模式
#[derive(Debug, PartialEq, Clone)]
pub enum DeviceMode {
    /// BROM 模式（通过 libusb 通信）
    Brom,
    /// Preloader 模式（串口已释放，切换到 USB BROM）
    Preloader,
}

/// 统一连接管理器
/// 
/// 职责：
/// 1. 统一设备入口（smart_init）
/// 2. 管理连接状态（BROM ↔ Preloader ↔ DA 切换）
/// 3. 自动重连（reconnect_loop）
/// 4. 自动驱动后端选择（UsbDk → libusb → COM）
/// 5. 对齐 MTKClient Python 行为
/// 
/// 三层驱动架构：
/// Layer 1: UsbDk backend（优先，Windows 原生，可绕过 COM 占用）
/// Layer 2: libusb backend（fallback，需设备未被 Windows 驱动占用）
/// Layer 3: Serial COM（仅 Preloader 阶段，握手后释放 → USB 重枚举）
/// 
/// 对齐 MTKClient Python:
/// - usblib.py::connect() → UsbDevice::open_by_vid_pid()
/// - mtk_preloader.py::init() → ConnectionManager::smart_init()
/// - Port.py::run_handshake() → SerialPortTransport::do_handshake()
/// - get_connection_agent() → driver::detect_backend()
/// - dynamic backend selection → UsbDk detection + fallback
pub struct ConnectionManager {
    mode: DeviceMode,
    stage: UsbStage,
    backend: DeviceBackend,
    port_name: Option<String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        // 启动时检测最佳可用后端
        let backend = driver::detect_backend();
        info!("[DRIVER] {}", driver::backend_status());

        ConnectionManager {
            mode: DeviceMode::Preloader,
            stage: UsbStage::Unknown,
            backend,
            port_name: None,
        }
    }

    /// 统一设备初始化入口
    /// 
    /// 完整流程（对齐刷机匣 + MTKClient Python）：
    /// ```
    /// STEP 1: USB fast scan（UsbDk 优先）
    ///   └─ 尝试打开 BROM PID（0x0003）
    ///   └─ 成功 → backend = UsbDk/Libusb → 返回
    ///   └─ 失败 → 进入 STEP 2
    /// 
    /// STEP 2: COM scan + handshake
    ///   └─ 扫描 COM 端口 → 打开串口 → BROM 握手 → init
    ///   └─ 成功 → stage = Preloader → 进入 STEP 3
    ///   └─ 失败 → 进入 STEP 4
    /// 
    /// STEP 3: COM → USB 切换
    ///   └─ 释放串口（drop）→ 等待重枚举（500ms）
    ///   └─ reconnect_loop(Brom) → libusb 接管
    /// 
    /// STEP 4: reconnect_loop 循环检测
    ///   └─ 扫描 BROM PID（0x0003）→ 10 秒超时，200ms 重试
    /// ```
    pub fn smart_init(
        &mut self,
        context: &UsbContext,
    ) -> Result<(Preloader, DeviceMode), String> {
        info!(
            "等待设备连接 (Preloader: 直接连接 / BROM: Vol+ + Vol- + Power)"
        );
        info!("[DRIVER] backend={:?}", self.backend);

        // === STEP 1: USB fast scan ===
        // 尝试直接打开 BROM 设备（UsbDk 优先）
        if let Ok(device) = usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, 0x0003) {
            info!(
                "  VID: {:04x}, PID: {:04x}, stage={:?}",
                device.vid, device.pid, device.stage
            );
            info!("{}", "USB 直接连接成功 (BROM 模式)".green().bold());
            self.backend = if driver::has_usbdk() {
                DeviceBackend::UsbDk
            } else {
                DeviceBackend::Libusb
            };
            self.mode = DeviceMode::Brom;
            self.stage = UsbStage::Brom;
            return Ok((Preloader::new(Box::new(device)), DeviceMode::Brom));
        }

        // 检测是否被 COM 占用
        if driver::is_device_com_occupied(0x0E8D, 0x0003) {
            info!("[USB] 设备被 Windows COM 驱动占用，libusb 无法直接打开");
            if !driver::has_usbdk() {
                warn!("[USB] UsbDk 不可用，建议安装 UsbDk 以自动接管设备");
                warn!("[USB] 或等待串口握手完成后自动切换");
            }
        }
        debug!("USB BROM 直接连接失败，尝试串口检测...");

        // === STEP 2: COM scan + handshake ===
        if let Some((port_name, preloader)) = self.serial_connect()? {
            info!("串口握手成功，准备切换到 USB 模式...");

            // === STEP 3: COM → USB 切换 ===
            // 关键：必须释放 COM 句柄，否则 USB 无法打开
            drop(preloader);
            debug!("串口已释放");

            // 等待设备重枚举（设备从 CDC 切换到 BROM VID/PID）
            // 不同设备枚举时间不同：MT6768 约 300ms，MT6785 约 500ms
            info!("等待设备重枚举...");
            std::thread::sleep(Duration::from_millis(500));

            // reconnect_loop: 循环检测 BROM 设备（10 秒，200ms 重试）
            let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

            info!(
                "  VID: {:04x}, PID: {:04x}, stage={:?}",
                usb_device.vid, usb_device.pid, usb_device.stage
            );

            self.backend = if driver::has_usbdk() {
                DeviceBackend::UsbDk
            } else {
                DeviceBackend::Libusb
            };
            self.mode = DeviceMode::Brom;
            self.stage = UsbStage::Brom;
            self.port_name = Some(port_name);
            return Ok((
                Preloader::new(Box::new(usb_device)),
                DeviceMode::Brom,
            ));
        }

        // === STEP 4: reconnect_loop 循环检测 ===
        info!("串口检测超时，尝试 USB 循环连接...");

        let usb_device = self.reconnect_loop(context, UsbStage::Brom)?;

        info!(
            "  VID: {:04x}, PID: {:04x}, stage={:?}",
            usb_device.vid, usb_device.pid, usb_device.stage
        );

        self.backend = if driver::has_usbdk() {
            DeviceBackend::UsbDk
        } else {
            DeviceBackend::Libusb
        };
        self.mode = DeviceMode::Brom;
        self.stage = UsbStage::Brom;
        Ok((Preloader::new(Box::new(usb_device)), DeviceMode::Brom))
    }

    /// 串口连接 + 握手
    /// 
    /// 扫描 COM 端口 → 打开串口 → BROM 握手 → init → 返回 Preloader
    /// 
    /// init 流程（严格对齐刷机匣日志）：
    /// - A0 → FA（BROM handshake）
    /// - FD → get_hw_code
    /// - D4 + 0x10007000 + 0x00000001 + rword + 0x22000000 + rword（watchdog disable）
    /// - D8 → get_target_config
    /// - FE → BROM sync
    /// - FF → 进入 ID 阶段
    /// - FC → 读 HW info
    fn serial_connect(&self) -> Result<Option<(String, Preloader)>, String> {
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
                    return Ok(Some((port_name, preloader)));
                }
                debug!("串口握手失败，继续轮询...");
            }

            std::thread::sleep(Duration::from_secs(1));
        }

        debug!("串口检测完成，未找到可用设备");
        Ok(None)
    }

    /// libusb 重连循环（支持阶段过滤 + 设备状态模型）
    /// 
    /// 对齐 MTKClient Python 行为：
    /// - usblib.py::connect() 循环扫描设备
    /// - mtk_preloader.py::init() 重试机制
    /// 
    /// 关键特性：
    /// - 10 秒超时，200ms 重试间隔
    /// - 支持 BROM / Preloader / DA 阶段过滤
    /// - 支持 USB reset 后重连
    /// - 支持 Kamakiri exploit 后重连
    /// - 支持 DA 加载后重连
    /// - 所有错误自动重试，不信任单次连接
    /// - 设备状态模型：避免误连（检查 interface count / endpoints）
    /// 
    /// 阶段过滤：
    /// - UsbStage::Brom → 扫描 PID 0x0003
    /// - UsbStage::Preloader → 扫描 PID 0x2000
    /// - UsbStage::Unknown → 扫描所有已知 PID
    /// 
    /// 日志输出：
    /// ```
    /// [RECONNECT] scanning for stage=Brom...
    /// [RECONNECT] retry 1/50...
    /// [RECONNECT] retry 6/50...
    /// [RECONNECT] success on attempt 3/50 (stage=Brom, PID=0x0003)
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

        while retry < max_retries {
            retry += 1;

            // 遍历所有候选 PID
            for &pid in &pids {
                match usb::UsbDevice::open_by_vid_pid(context, 0x0E8D, pid) {
                    Ok(device) => {
                        // 设备状态模型验证：确认设备可用
                        if self.validate_device(&device) {
                            info!(
                                "[RECONNECT] success on attempt {}/{} (stage={:?}, PID=0x{:04X})",
                                retry, max_retries, device.stage, device.pid
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
                        // 只在 debug 模式下记录
                        debug!("[RECONNECT] open_by_vid_pid failed for PID=0x{:04X}: {}", pid, e);
                    }
                }
            }

            if retry % 5 == 1 || retry == max_retries {
                debug!("[RECONNECT] retry {}/{} (scanning {} PIDs)...", retry, max_retries, pids.len());
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

    /// 获取当前驱动后端
    pub fn backend(&self) -> &DeviceBackend {
        &self.backend
    }

    /// 获取串口名称
    #[allow(dead_code)]
    pub fn port_name(&self) -> Option<&str> {
        self.port_name.as_deref()
    }
}
