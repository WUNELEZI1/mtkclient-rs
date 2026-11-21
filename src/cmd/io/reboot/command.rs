use crate::cmd::io::boot_mode::{set_boot_mode_via_para, set_bootloader_message};
use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};
use std::time::Duration;

use super::plan::{RebootAction, RebootNotice, plan_reboot};

/// Reboot the device
///
/// Usage:
///   reboot                          → normal reboot into the system
///   reboot fastboot                 → reboot to Bootloader (lk fastboot)
///   reboot recovery                 → reboot to Recovery
///   reboot fastbootd                → reboot to fastbootd (userspace fastboot)
///   reboot meta                     → reboot to META mode
///   reboot `<mode>` --via `<method>`    → specify the reboot method
///
/// --via options:
///   para      (default) set boot_mode via the para partition (most reliable, needs DA)
///   misc      set bootloader_message via the misc partition (needs DA)
///   da        reboot directly via the DA SHUTDOWN command (needs DA)
///   xml       reboot via XML DA SET-BOOT-MODE (new platforms, needs DA)
///   preloader reboot via the Preloader Pattern protocol (fastboot/meta, pure Preloader, no DA)
///
/// Design principle (the only pure-Preloader path):
///   -- Only `reboot <mode> --via preloader` is a pure Preloader path: it intercepts before DA load,
///      going straight through the Preloader Pattern protocol (fastboot/meta) / Preloader watchdog (system),
///      without loading DA or writing any partition.
///   -- All other reboots (default system, fastboot/recovery/fastbootd/meta, and
///      --via para/misc/da/xml):
///      always go through bypass→upload_da and finish via DA SHUTDOWN / DA commands; never pure Preloader.
///   -- Exception (fallback): if the device is already in DA mode (daext=true, including DA reuse / --mode preloader
///      DA fallback), the `--via preloader` Pattern protocol is unavailable because it depends on the original
///      handshake state, so it automatically degrades to the default DA reboot (write para/misc + DA SHUTDOWN)
///      with a notice, instead of taking the doomed Pattern path.
///
/// system reboot notes (reboot / reboot system, default path, part of "all other paths" → uses DA):
///   Measured on this device (MT6768), behavior of the two primitives:
///     - raw watchdog (BROM WRITE32 wdt) reliably reboots into the system when "no DA is active";
///     - DA SHUTDOWN(bootmode=HOME_SCREEN) is used when "DA is active", and **after a successful send the
///       device port must be closed immediately** (matching port.close(reset=True) at the end of mtkclient shutdown),
///       otherwise, after DA soft-jumps back to preloader, the device stays in download mode because the host
///       still holds USB/serial, showing "log says reboot succeeded but the device did not enter the system / no effect".
///   Branching: daext=false (DA not loaded, defensive fallback) → raw watchdog hard reset; daext=true (DA active, including reused session
///   or fresh upload_da) → DA SHUTDOWN + close_device. The command only loads/reuses DA when DA is active,
///   and writes no partition.
///
/// Modes supported by --via preloader (pure Preloader, ignores DA):
///   fastboot  → Pattern FASTBOOT (BROM: reset to Preloader first, then Pattern)
///   meta      → Pattern METAMETA (same as above)
///   system    → Preloader watchdog hard reset (also no DA load)
///   recovery/fastbootd → Pattern unsupported, fall back to para (DA will be loaded then)
///
/// Relationship between mode and working mode:
///   --mode brom / --mode preloader (reboot system, default, not --via preloader):
///     load DA → DA SHUTDOWN(bootmode=HOME_SCREEN) → reboot into the system
///   --mode brom:
///     --via preloader: pure Preloader (BROM write32 triggers watchdog → wait for Preloader → handshake → Pattern)
///     --via para/misc/da/xml: load DA → write partition → reboot
///   --mode preloader:
///     --via preloader: pure Preloader (trigger_meta_reboot → Pattern)
///     --via para/misc: load DA → write partition → reboot
pub fn cmd_reboot(
    da: &mut DAXFlash,
    args: &[String],
    is_brom: bool,
    is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse args: reboot [mode] [--via misc/para/da/xml/preloader]
    let mut mode = "system";
    let mut via = "para"; // default to para (most reliable)

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--via" => {
                via = args.get(i + 1).map(|s| s.as_str()).unwrap_or("para");
                i += 2;
            }
            "system" | "fastboot" | "recovery" | "fastbootd" | "meta" => {
                mode = args[i].as_str();
                i += 1;
            }
            _ => {
                return Err(format!(
                    "未知重启参数: {}。用法: reboot [system|fastboot|recovery|fastbootd|meta] [--via misc|para|da|xml|preloader]",
                    args[i]
                )
                .into());
            }
        }
    }

    // Pending-read guard: if there is an active resumable read (interrupted by Ctrl+C before completion),
    // reboot would destroy the DA session, losing the read progress; with a reused session DA SHUTDOWN
    // also often fails with 0x00010007. Keep the session and ask the user to finish the read first
    // instead of attempting DA SHUTDOWN.
    let pending = crate::cmd::pending_read_resume_cwd();
    if !pending.is_empty() {
        warn!(
            "{}",
            "检测到未完成的读取任务，已跳过重启以保持 DA 会话"
                .yellow()
                .bold()
        );
        for p in &pending {
            let pct = if p.size > 0 {
                format!(
                    "（{:.1}% 已完成）",
                    p.written as f64 / p.size as f64 * 100.0
                )
            } else {
                String::new()
            };
            warn!(
                "  - {}：已读取 {}/{} 字节 {}",
                p.output, p.written, p.size, pct
            );
        }
        warn!(
            "{}",
            "请先完成分区读取（重新运行对应的 r <分区> <文件> 命令，会自动从断点续传）后再重启。"
                .yellow()
        );
        warn!(
            "{}",
            "若确定要放弃读取并强制重启，请先删除对应的 .img 与 .resume 文件后重试。".dimmed()
        );
        return Ok(());
    }

    // The whole via × mode × DA-state decision is made by the pure planner.
    let plan = plan_reboot(via, mode, da.daext, is_brom, is_preloader);

    // Emit the degradation notices the planner recorded, in order.
    for notice in &plan.notices {
        match notice {
            RebootNotice::DaDegradedPreloader { mode } => {
                warn!(
                    "{}",
                    format!(
                        "设备处于 DA 模式，--via preloader 的 Pattern 协议不可用；--mode 仍为 {mode}，已降级为经 DA 重启（写 para/misc + DA SHUTDOWN）"
                    )
                    .yellow()
                );
            }
            RebootNotice::PreloaderViaUnsupported { via } => {
                warn!(
                    "Preloader 模式下 via={} 不支持（仅支持 misc/para/preloader），已回退到 para",
                    via
                );
            }
        }
    }

    match plan.action {
        RebootAction::PreloaderPattern => {
            return cmd_reboot_via_preloader(da, mode, is_brom, is_preloader);
        }
        RebootAction::SystemDaShutdown => {
            // system reboot: prefer DA SHUTDOWN(bootmode=HOME_SCREEN) — the only way MTK can
            // reliably reboot into the system over USB. A raw watchdog hard reset makes the device
            // drop back to download mode ("log says reboot succeeded but the device did not boot"),
            // so it is only a fallback when DA is unavailable.
            //
            // daext=true (DA loaded, either just uploaded or a reused session): DA SHUTDOWN directly.
            // daext=false (extreme: DA failed to load): degrade to raw watchdog hard reset with a clear warning.
            info!("通过 DA SHUTDOWN 触发重启到系统（bootmode=HOME_SCREEN）...");
            match da.reset_device() {
                Ok(_) => {
                    info!("{}", "设备正在重启进入系统...".green());
                    // Critical: DA SHUTDOWN is a soft jump. After DA jumps back to preloader,
                    // if the host still holds USB/serial (and the boot reason is download mode)
                    // the device stalls in download mode, appearing as "no effect".
                    // Explicitly close the device port (matching port.close(reset=True) at the end
                    // of mtkclient shutdown) to release the handle so the device re-enumerates cleanly and boots.
                    let _ = da.preloader.device.close_device();
                    // Wait for the physical reset and re-enumeration: after MTK DA receives SHUTDOWN it jumps
                    // back to preloader and then boots into the system; the host must yield the USB bus long
                    // enough (matching close + time.sleep at the end of mtkclient shutdown). 500ms is too short:
                    // the OS keeps a stale handle and the device stalls in download mode (log shows success but no reboot).
                    std::thread::sleep(Duration::from_millis(2000));
                    crate::connection::reset_session();
                    info!("{}", "请保持或断开 USB，等待设备启动到系统".cyan());
                }
                Err(e) => {
                    // The raw watchdog is unusable in DA mode: the BROM echo protocol is gone once DA
                    // takes over, so it always fails with meta_reset: echo 0xD4 mismatch; using the raw
                    // watchdog only produces misleading logs. Report the failure and reset the session so the
                    // user can rerun this command: the manager will re-handshake with Preloader and reload DA,
                    // which restores normal reboot in most cases.
                    warn!("DA SHUTDOWN 失败: {}", e);
                    crate::connection::reset_session();
                    info!(
                        "{}",
                        "DA 重启失败，请重新运行本 reboot 命令（将重新握手并加载 DA 后再试）"
                            .yellow()
                    );
                }
            }
        }
        RebootAction::SystemWatchdog => {
            warn!(
                "{}",
                "DA 未加载，退化为裸看门狗硬复位（USB 连接下设备可能重新进入下载模式）"
                    .yellow()
                    .bold()
            );
            watchdog_reboot(da)?;
        }
        RebootAction::FastbootXml => {
            info!("通过 XML DA 协议重启到 fastboot...");
            da.da_xml_reboot_fastboot()
                .map_err(|e| format!("XML DA fastboot 失败: {}", e))?;
            info!("{}", "设备已通过 XML DA 重启到 fastboot".green());
            // Close the device port (matching close at the end of mtkclient shutdown)
            let _ = da.preloader.device.close_device();
            std::thread::sleep(Duration::from_millis(2000));
            crate::connection::reset_session();
            return Ok(());
        }
        RebootAction::FastbootPara => {
            set_boot_mode_via_para(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::FastbootMisc => {
            set_bootloader_message(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::RecoveryPara => {
            info!("准备重启到 {} 模式 (via {})...", mode, plan.via);
            set_boot_mode_via_para(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::RecoveryMisc => {
            info!("准备重启到 {} 模式 (via {})...", mode, plan.via);
            set_bootloader_message(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::RecoveryXmlFallback => {
            info!("准备重启到 {} 模式 (via {})...", mode, plan.via);
            warn!("XML DA 不支持 {} 模式，回退到 misc", mode);
            set_bootloader_message(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::MetaDa => {
            info!("准备重启到 meta 模式 (via {})...", plan.via);
            info!("通过 DA SET_META_BOOT_MODE 重启到 meta...");
            if let Err(e) = da.set_meta_boot_mode(1) {
                warn!("SET_META_BOOT_MODE 失败: {}，回退到 misc", e);
                set_bootloader_message(da, mode)?;
            }
            do_reboot(da, mode)?;
        }
        RebootAction::MetaXml => {
            info!("准备重启到 meta 模式 (via {})...", plan.via);
            da.da_xml_reboot_meta()
                .map_err(|e| format!("XML DA meta 失败: {}", e))?;
            info!("{}", "设备已通过 XML DA 重启到 meta".green());
            // Close the device port (matching close at the end of mtkclient shutdown)
            let _ = da.preloader.device.close_device();
            std::thread::sleep(Duration::from_millis(500));
            crate::connection::reset_session();
            return Ok(());
        }
        RebootAction::MetaPara => {
            info!("准备重启到 meta 模式 (via {})...", plan.via);
            set_boot_mode_via_para(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::MetaMisc => {
            info!("准备重启到 meta 模式 (via {})...", plan.via);
            set_bootloader_message(da, mode)?;
            do_reboot(da, mode)?;
        }
        RebootAction::UnsupportedMode => {
            return Err(format!("不支持的重启模式: {}", mode).into());
        }
    }

    Ok(())
}

/// --via preloader: reboot the device via the Pattern protocol
///
/// Supported modes: fastboot, meta
/// Unsupported modes (recovery/fastbootd/system): fall back to para
///
/// BROM flow (intercepted before DA load, device still in BROM handshake state):
/// 1. trigger_meta_reboot: write32(wdt+0x14, 0x00001209) triggers a watchdog reboot
/// 2. close the serial port → wait for the device to re-enumerate as Preloader VCOM
/// 3. open_raw → read READY → send the mode identifier → DISCONNECT
///
/// Preloader flow (same as above)
fn cmd_reboot_via_preloader(
    da: &mut DAXFlash,
    mode: &str,
    is_brom: bool,
    _is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cmd::preloader_boot_mode::{self, BootMode};

    // Mode mapping supported by the Pattern protocol
    let boot_mode = match mode {
        "fastboot" => BootMode::Fastboot,
        "meta" => BootMode::Meta,
        _ => {
            // recovery/fastbootd/system do not support Pattern; fall back to para
            warn!(
                "{} 模式不支持 --via preloader（Pattern 协议无对应标识），回退到 --via para",
                mode
            );
            match mode {
                "system" => {
                    // system mode: always use the hardware watchdog hard reset (same as default reboot system)
                    info!("通过看门狗硬复位重启到系统...");
                    if da.preloader.chip.is_none() {
                        if let Err(e) = da.preloader.get_hw_code() {
                            warn!("获取芯片信息失败（{}），无法触发看门狗", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                            return Ok(());
                        }
                    }
                    match da.preloader.trigger_meta_reboot() {
                        Ok(_) => {
                            info!("{}", "看门狗已触发，设备正在硬件复位...".green());
                            crate::connection::reset_session();
                        }
                        Err(e) => {
                            warn!("看门狗触发失败: {}", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                        }
                    }
                }
                "recovery" | "fastbootd" | "meta" => {
                    set_boot_mode_via_para(da, mode)?;
                    do_reboot(da, mode)?;
                }
                _ => {}
            }
            return Ok(());
        }
    };

    let mode_name = boot_mode.name();
    let tag = if is_brom { "BROM" } else { "Preloader" };
    info!("通过 Preloader Pattern 协议重启到 {}...", mode_name);

    // Step 1: trigger_meta_reboot triggers a watchdog reboot
    info!("  [{}] 步骤 1/2: 触发看门狗重启...", tag);
    match da.preloader.trigger_meta_reboot() {
        Ok(_) => info!("  看门狗已触发，设备正在重启..."),
        Err(e) => warn!("  看门狗触发失败: {}，继续等待设备重枚举", e),
    }
    let _ = da.preloader.device.close_device();
    crate::connection::reset_session();
    std::thread::sleep(Duration::from_millis(1000));

    // Step 2: wait for Preloader VCOM → Pattern protocol
    info!(
        "  [{}] 步骤 2/2: 等待 Preloader VCOM → Pattern 协议...",
        tag
    );
    match preloader_boot_mode::try_preloader_pattern(boot_mode) {
        Ok(_) => {
            info!(
                "{}",
                format!("  设备已通过 Pattern 重启到 {}", mode_name)
                    .green()
                    .bold()
            );
            return Ok(());
        }
        Err(e) => {
            warn!("  Pattern 发送失败: {}，设备可能已进入目标模式", e);
            return Ok(());
        }
    }
}

/// Raw watchdog hard-reset fallback (MTK BROM WRITE32 wdt+0x14 = 0x1209).
///
/// Note: over USB the raw watchdog makes the device drop back to Preloader/BROM download mode
/// instead of booting the system, so use it only as a last resort when DA SHUTDOWN is unavailable,
/// and warn the user about this risk.
fn watchdog_reboot(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    if da.preloader.chip.is_none() {
        if let Err(e) = da.preloader.get_hw_code() {
            warn!("获取芯片信息失败（{}），无法触发看门狗", e);
            crate::connection::reset_session();
            info!(
                "{}",
                "请手动重启设备（断开 USB 重新连接或按电源键）".yellow()
            );
            return Ok(());
        }
    }
    match da.preloader.trigger_meta_reboot() {
        Ok(_) => {
            info!("{}", "看门狗已触发，设备正在硬件复位...".green());
            crate::connection::reset_session();
            info!(
                "{}",
                "若设备仍停在下载模式（VCOM/Preloader），请手动重启或断开 USB 后重连".cyan()
            );
        }
        Err(e) => {
            warn!("看门狗触发失败: {}", e);
            crate::connection::reset_session();
            info!(
                "{}",
                "请手动重启设备（断开 USB 重新连接或按电源键）".yellow()
            );
        }
    }
    Ok(())
}

/// Perform the actual reboot
/// Uses the flasher protocol (SYNC + CMD_SHUTDOWN); on failure, ask the user to reboot manually
fn do_reboot(da: &mut DAXFlash, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
            // Same as reboot system: DA SHUTDOWN is a soft jump, so the device port must be closed
            // to release the handle; otherwise the device stalls in download mode (host still holds USB),
            // appearing as "no effect". Matching mtkclient close.
            let _ = da.preloader.device.close_device();
            std::thread::sleep(Duration::from_millis(500));
            info!("{}", "请断开 USB 连接，等待设备自动重启".cyan());
            crate::connection::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("重启失败: {}", e);
            crate::connection::reset_session();
            info!("{}", format!(
                "misc/para 分区已写入，请手动重启设备（断开 USB 重新连接或按电源键重启），设备将启动到 {} 模式",
                mode
            ).yellow());
            Ok(())
        }
    }
}
