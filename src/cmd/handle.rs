//! Single-command execution entry (handle_command)
//!
//! Handles Phase1 (preloader dump / Kamakiri2 bypass / EMI load / upload_da)
//! and Phase2 (DA command dispatch); it is the top-level entry called directly by main.rs.

use crate::color::Colorize;
use log::{debug, info, warn};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;
use crate::usb::UsbContext;

/// Single-command execution entry
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

    // Precheck: validate the command before loading the DA, avoiding wasting time only to find it invalid
    super::validate_command(cmd, &app_config.cmd_args)?;

    // reboot command branching:
    //   - Only --via preloader must be intercepted before DA load (it depends on the raw Preloader/BROM handshake
    //     Pattern protocol: trigger the watchdog first, then handshake with Preloader to send the mode marker).
    //   - All other reboot modes (system/fastboot/recovery/fastbootd/meta) fall through,
    //     letting handle_command finish bypass→dump→EMI→upload_da, then reboot reliably into the system
    //     inside cmd_reboot via DA SHUTDOWN(bootmode=HOME_SCREEN).
    //
    // Why not use a bare watchdog as the system default: under a USB connection, an MTK bare watchdog hardware reset
    // drops back into Preloader/BROM download mode instead of booting the system (symptom: "log says reboot succeeded but device did not boot").
    // Only DA SHUTDOWN can cleanly make the device skip download mode and boot straight into the system, matching mtkclient.
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
        // --via preloader must run before DA load (depends on the raw handshake Pattern protocol).
        // But if the device is already in DA mode (daext=true, including DA reuse/fallback), the Pattern protocol is unavailable;
        // in that case do not intercept early and fall through to the DA path below (cmd_reboot falls back to the DA default reboot).
        if has_via_preloader && !da.daext {
            return super::io::cmd_reboot(da, args, is_brom, !is_brom);
        }
    }

    let has_active_read_resume =
        cmd == "r" && super::active_read_resume_exists(&app_config.cmd_args);
    let has_active_write_resume =
        cmd == "w" && super::active_write_resume_exists(&app_config.cmd_args);

    // Write resume: the DA is still running in DRAM, but the USB state may be inconsistent (a prior cancel left endpoint residue).
    // Keep the DA session (do not reset), but re-sync later in handle_command via drain + xflash_sync.
    // Do not reset_session() + reload the DA: the device is already in DA mode, so the BROM handshake would fail.
    if has_active_write_resume {
        debug!("[DA_SESSION] 检测到写入续传，保持 DA 会话，将重新同步 USB 通信");
    }

    // Skip preloader dump / bypass / EMI load when reusing a DA session (the DA is still running)
    if !da.daext && is_brom {
        match cmd {
            "dumppreloader" => {
                super::dump::cmd_dumppreloader(da, Some(_context))?;
                return Ok(());
            }
            _ => {}
        }

        // 1. Get the target config to decide whether a bypass is needed
        let has_security = match da.preloader.get_target_config() {
            Ok(cfg) => {
                info!("{}", cfg.format_info());
                if cfg.requires_auth() {
                    info!("设备启用了 SLA/DAA 认证");
                }
                cfg.needs_bypass()
            }
            Err(e) => {
                warn!("获取 target config 失败: {}", e);
                warn!("假设 bypass 已处理，直接继续...");
                false
            }
        };

        // Automatic preloader extraction (without --preloader) relies on read32(0xD1) to read the preloader from RAM.
        // MTK BROM's SRAM read restriction is unrelated to the SBC/SLA/DAA security flags: even with 0x0 unprotected,
        // read32 may still be rejected by BROM (observed: `--mode brom printgpt` auto-extraction reports
        // "read32 cannot access this memory region"), so Kamakiri2 must first lift the memory read restriction.
        // bypass_security re-handshakes after injecting the patcher and restores BROM state without polluting the later DA upload,
        // so bypass is forced only in the narrow "auto-extract" case; the general DA load path is unaffected.
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

        // 2. If no preloader file is given, automatically extract the preloader from RAM (non-destructive read32).
        //    The auto_extract case above already forced Kamakiri2 in a narrow scope, so post-bypass read32 can
        //    read the preloader normally; if device memory is still unreadable (very rare, e.g. incompatible patcher), give clear guidance
        //    telling the user to use dumppreloader or --preloader.
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

    // In preloader mode DRAM is already initialized by the preloader, so EMI data is optional
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

    // DA session reuse: use it directly without heartbeat verification.
    // Flash-tool logs confirm the DA state machine is intact after USB reopen; a direct devctrl query suffices.
    // Heartbeat verification may time out right after USB reopen and be misjudged, triggering a destructive reconnect/fallback.
    if da.daext {
        if has_active_read_resume {
            debug!("[DA_SESSION] 检测到活跃读取续传，直接续接数据流");
        } else if has_active_write_resume {
            // Write resume: the USB connection has just reopened; drain residue + re-sync the DA state machine
            debug!("[DA_SESSION] 检测到写入续传状态，重新同步 DA 通信...");
            da.preloader.device.drain_pipes();
            // Send xflash_sync to re-sync (a write cancel may leave the DA waiting for an unfinished write-packet response)
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

    // multi and adb both run after the DA session is established and do not go through execute_single_command
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
        // Session reset policy (key fix): reset the session only when the DA/USB transport layer is truly broken.
        //
        // The old logic used string prefixes ("unknown command"/"usage:") to detect "non-communication errors", but once wrapped by AppError
        // Display adds prefixes like "protocol error:"/"USB error:", so starts_with checks completely fail,
        // causing almost all command-level errors (bad args, partition not found, file error, parse error, security error) to be misjudged
        // as DA communication errors and reset the session — unnecessary, forcing a re-handshake/Bypass next time and wasting time.
        //
        // New logic: keep the session by default; reset only when the error is truly a transport-layer failure (USB disconnect, DA timeout, endpoint error, etc.).
        // Criteria: ① user cancel (Ctrl+C) → keep; ② AppError::Usb (real transport failure) → reset;
        // ③ other AppError (Protocol/Security/Parse/Io) and business errors → keep, reset only if the message contains transport keywords.
        let err_str = e.to_string();
        let is_user_cancel = crate::cancel::requested() || crate::cancel::force_requested();
        let is_transport_error = super::is_transport_error(&*e);
        if is_transport_error && !is_user_cancel {
            warn!("[DA_SESSION] DA 通信错误，重置会话状态: {}", err_str);
            crate::connection::reset_session();
            // Clean up any leftover read resume file to avoid an infinite loop on the next startup
            // Note: the write resume file is not cleaned here; keep it for checkpoint resume
            if cmd == "r" {
                if let Some(output) = app_config.cmd_args.get(1) {
                    let resume_path = crate::resume::read_resume_path(output);
                    let _ = std::fs::remove_file(&resume_path);
                }
            }
        } else if !is_user_cancel {
            // Non-transport error: the DA session itself is healthy; only log, do not reset
            debug!(
                "[DA_SESSION] 非传输错误，保留 DA 会话（不重置）: {}",
                err_str
            );
        }
        e
    })?;

    Ok(())
}
