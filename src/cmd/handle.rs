//! 单命令执行入口（handle_command）
//!
//! 处理 Phase1（preloader dump / Kamakiri2 bypass / EMI 加载 / upload_da）
//! 与 Phase2（DA 命令分发），是 main.rs 直接调用的顶层入口。

use crate::color::Colorize;
use log::{debug, info, warn};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;
use crate::usb::UsbContext;

/// 单命令执行入口
pub fn handle_command(
    da: &mut DAXFlash,
    app_config: &AppConfig,
    log_level: u8,
    _quiet_dump: bool,
    preloader_file: &str,
    _context: &UsbContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    let cmd = app_config.command.as_deref().unwrap_or("");

    // 预检查：在 DA 加载之前验证命令有效性，避免浪费时间后才发现命令错误
    super::validate_command(cmd, &app_config.cmd_args)?;

    // reboot 命令分流：
    //   - 仅 --via preloader 需要在 DA 加载前拦截（依赖原始 Preloader/BROM 握手态的
    //     Pattern 协议：先触发看门狗再握手 Preloader 发模式标识）。
    //   - 其余 reboot 模式（system/fastboot/recovery/fastbootd/meta）一律放过，
    //     由 handle_command 完成 bypass→dump→EMI→upload_da 后，在 cmd_reboot 内
    //     通过 DA SHUTDOWN(bootmode=HOME_SCREEN) 可靠重启进系统。
    //
    // 为何不再用裸看门狗作为 system 默认：MTK 在 USB 连接下裸看门狗硬件复位会重新掉回
    // Preloader/BROM 下载模式而非进系统（表现为"日志显示重启成功但设备没进系统"）。
    // 只有 DA SHUTDOWN 能干净地让设备跳过下载态直接 boot 到系统，行为对齐 mtkclient。
    if cmd == "reboot" {
        let mut has_via_preloader = false;
        let args = &app_config.cmd_args;
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--via" => {
                    if let Some(v) = args.get(i + 1) {
                        if v == "preloader" {
                            has_via_preloader = true;
                        }
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
        // --via preloader 必须在 DA 加载前执行（依赖原始握手态的 Pattern 协议）。
        // 但若设备已处于 DA 模式（daext=true，含 DA 复用/fallback），Pattern 协议不可用，
        // 此时不放行提前拦截，改走下方 DA 路径（cmd_reboot 内部会降级为 DA 默认重启）。
        if has_via_preloader && !da.daext {
            return super::io::cmd_reboot(da, args, is_brom, !is_brom);
        }
    }

    let has_active_read_resume = cmd == "r" && super::active_read_resume_exists(&app_config.cmd_args);
    let has_active_write_resume = cmd == "w" && super::active_write_resume_exists(&app_config.cmd_args);

    // 写入续传：DA 仍在 DRAM 中运行，但 USB 状态可能不一致（上次取消导致 endpoint 残留）
    // 保持 DA 会话复用（不重置），但在后续 handle_command 中通过 drain + xflash_sync 重新同步
    // 不能 reset_session() + 重新加载 DA，因为设备已在 DA 模式，BROM 握手会失败
    if has_active_write_resume {
        debug!("[DA_SESSION] 检测到写入续传，保持 DA 会话，将重新同步 USB 通信");
    }

    // DA 会话复用时跳过 preloader dump / bypass / EMI 加载（DA 仍在运行）
    if !da.daext && is_brom {
        match cmd {
            "dumppreloader" => {
                super::dump::cmd_dumppreloader(da, Some(_context))?;
                return Ok(());
            }
            _ => {}
        }

        // 1. 获取 target config 判断是否需要 bypass
        let has_security = match da.preloader.get_target_config() {
            Ok(cfg) => {
                info!("{}", cfg.format_info());
                cfg.needs_bypass()
            }
            Err(e) => {
                warn!("获取 target config 失败: {}", e);
                warn!("假设 bypass 已处理，直接继续...");
                false
            }
        };

        // 自动提取 preloader（未指定 --preloader）依赖 read32(0xD1) 从 RAM 读 preloader。
        // MTK BROM 对 SRAM 的读取限制与 SBC/SLA/DAA 这套安全标志无关：即便 0x0 无保护，
        // read32 仍可能被 BROM 拒绝（实测 --mode brom printgpt 自动提取报
        // "read32 无法访问该内存区域"），必须先用 Kamakiri2 解除内存读取限制。
        // bypass_security 注入 patcher 后会重新握手恢复 BROM 状态，不污染后续 DA 上传，
        // 因此仅在"需要自动提取"时窄范围强制 bypass，通用 DA 加载路径不受影响。
        let auto_extract = preloader_file.is_empty();
        let needs_bypass = has_security || auto_extract;

        if needs_bypass {
            if auto_extract && !has_security {
                info!(
                    "未指定 --preloader，需自动提取；设备虽为 0x0 无保护，但 read32 仍受 BROM 限制，先执行 Kamakiri2 解锁内存读取"
                );
            } else if has_security {
                info!("设备有安全保护，执行 Kamakiri2 bypass...");
            }
            da.preloader
                .bypass_security(_context)
                .map_err(|e| format!("bypass_security 失败: {}", e))?;
        } else {
            info!(
                "设备无安全保护（SBC/SLA/DAA/MemRead 全关）且已指定 --preloader，跳过 Kamakiri2，直接进入 DA 模式"
            );
        }

        // 2. 如果没有指定 preloader 文件，自动从 RAM 提取 preloader（非破坏性 read32）。
        //    上方已对 auto_extract 场景窄范围强制 Kamakiri2，post-bypass 的 read32 可正常
        //    读取 preloader；若设备内存仍不可读（极罕见，如 patcher 不兼容），给出明确指引
        //    让用户用 dumppreloader 或 --preloader。
        if preloader_file.is_empty() {
            info!("未指定 --preloader，自动从 RAM 提取 preloader...");
            match da.preloader.dump_preloader_via_brom_read() {
                Ok((data, filename)) => {
                    auto_dumped_file = Some(filename.clone());
                    info!("Preloader 已自动提取: {} ({} 字节)", filename, data.len());
                }
                Err(e) => {
                    return Err(format!(
                        "自动提取 preloader 失败: {}。\n\
                         请手动运行 'dumppreloader' 命令获取文件，\n\
                         或使用 --preloader <文件> 参数指定 preloader 文件。\n\
                         提示: preloader 文件通常位于固件包的 'images' 目录中，\n\
                         文件名类似 preloader_*.bin。",
                        e
                    )
                    .into());
                }
            }
        }
    }

    // Preloader 模式下 DRAM 已由 preloader 初始化，EMI 数据可选
    let effective_preloader = if !preloader_file.is_empty() {
        Some(preloader_file.to_string())
    } else {
        auto_dumped_file.clone()
    };

    if let Some(ref f) = effective_preloader {
        info!("加载 EMI 数据: {}", f);
        if let Err(e) = da.load_preloader_emi(f) {
            return Err(format!("EMI 加载失败: {}", e).into());
        }
        da.preloader_path = Some(f.clone());
    } else if da.preloader.is_preloader_mode {
        debug!("Preloader 模式：跳过 EMI 加载（DRAM 已由 preloader 初始化）");
    } else {
        return Err("未找到 preloader 文件，且自动提取失败".into());
    }

    // DA 会话复用：直接使用，不做心跳验证
    // 刷机匣日志证实 USB 重新打开后 DA 状态机完整，直接 devctrl 查询即可。
    // 心跳验证在 USB 刚重开时可能超时导致误判，触发破坏性的 reconnect/fallback 流程。
    if da.daext {
        if has_active_read_resume {
            debug!("[DA_SESSION] 检测到活跃读取续传，直接续接数据流");
        } else if has_active_write_resume {
            // 写入续传：USB 连接刚重新打开，需要排空残留 + 重新同步 DA 状态机
            debug!("[DA_SESSION] 检测到写入续传状态，重新同步 DA 通信...");
            da.preloader.device.drain_pipes();
            // 发送 xflash_sync 重新同步（写入取消可能导致 DA 等待未完成的写包响应）
            match da.xflash_sync() {
                Ok(_) => debug!("[DA_SESSION] DA 同步成功，可以续写"),
                Err(e) => {
                    warn!("[DA_SESSION] DA 同步失败: {}，尝试 drain 后继续", e);
                    da.preloader.device.drain_pipes();
                }
            }
        } else {
            debug!("DA 会话复用，直接执行命令");
        }
    } else {
        da.upload_da().map_err(|e| format!("DA 加载失败: {}", e))?;
    }

    if log_level >= 2 {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write(crate::system::paths::get_tmp_path("emi_debug.bin"), data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write(
                crate::system::paths::get_tmp_path("extensions_debug.bin"),
                &data,
            );
        }
    }

    let args = &app_config.cmd_args;
    let verify = app_config.verify;

    // multi 和 adb 都在 DA 会话已建立后执行，不需要走 execute_single_command
    if cmd == "adb" {
        da.enable_adb_and_reboot()?;
        info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        return Ok(());
    }

    if cmd == "multi" {
        super::multi::cmd_multi(da, args, app_config, log_level)?;
        return Ok(());
    }

    if cmd == "run" {
        super::script::cmd_run(da, args, app_config, log_level)?;
        return Ok(());
    }

    super::execute_single_command(da, cmd, args, verify, log_level, app_config).map_err(|e| {
        // 会话重置策略（关键修正）：只有 DA/USB 传输层真正失效时才重置会话。
        //
        // 旧逻辑靠字符串前缀（"未知命令"/"用法:"）判断"非通信错误"，但错误经 AppError
        // 包裹后 Display 会加"协议错误:""USB 错误:"等前缀，starts_with 判断完全失效，
        // 导致几乎所有命令级错误（参数错、分区找不到、文件错、解析错、安全错）都被误判为
        // DA 通信错误而重置会话——这是没必要的，会强制下次重新握手/Bypass，浪费时间。
        //
        // 新逻辑：默认保留会话；仅当错误确为传输层失败（USB 断连、DA 超时、端点错误等）才重置。
        // 判定：① 用户取消(Ctrl+C) → 保留；② AppError::Usb（真实传输失败）→ 重置；
        // ③ 其余 AppError(Protocol/Security/Parse/Io)与业务错误 → 保留，仅当消息含传输关键词才重置。
        let err_str = e.to_string();
        let is_user_cancel = crate::cancel::requested() || crate::cancel::force_requested();
        let is_transport_error = super::is_transport_error(&*e);
        if is_transport_error && !is_user_cancel {
            warn!("[DA_SESSION] DA 通信错误，重置会话状态: {}", err_str);
            crate::connection::reset_session();
            // 清理可能残留的 read resume 文件，避免下次启动死循环
            // 注意：write resume 文件不在此时清理，保留供断点续写使用
            if cmd == "r" {
                if let Some(output) = app_config.cmd_args.get(1) {
                    let resume_path = crate::resume::read_resume_path(output);
                    let _ = std::fs::remove_file(&resume_path);
                }
            }
        } else if !is_user_cancel {
            // 非传输错误：DA 会话本身健康，仅记录日志，不重置
            debug!(
                "[DA_SESSION] 非传输错误，保留 DA 会话（不重置）: {}",
                err_str
            );
        }
        e
    })?;

    Ok(())
}
