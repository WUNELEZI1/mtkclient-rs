//! Unified command dispatch (dispatch_cmd / execute_single_command)
//!
//! `dispatch_cmd` is the single source of all command match logic, called by `execute_single_command`,
//! `multi::cmd_multi` and `script::dispatch_command` so all three share the same command list and logic.

use crate::color::Colorize;
use log::{error, info};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

/// Unified command dispatch function (the single source of all command match logic)
///
/// Called by `execute_single_command`, `multi::cmd_multi` and `script::dispatch_command`
/// so all three share exactly the same command list and logic.
/// Execute an op through the capability-driven path (on real hardware).
///
/// Uses the same chain as `printgpt`: RealSession (DA already uploaded by Phase1) → Executor →
/// Orchestrator decisions → step execution + capability checks. Confirmation comes from `--yes` (auto-approve) or interactive y/N.
pub(crate) fn run_op<O: crate::ops::Op>(
    da: &mut DAXFlash,
    op: &O,
    app_config: &AppConfig,
    log_level: u8,
) -> Result<(), crate::error::AppError> {
    let state = real_device_state(da);
    let mode = crate::core::plan::Mode::parse(mode_label(app_config))
        .unwrap_or(crate::core::plan::Mode::Auto);
    let mut session = crate::session::RealSession::new(da, state, true);
    crate::exec::Executor::new(mode)
        .with_assume_yes(app_config.assume_yes)
        .execute(op, &mut session, true, log_level)?;
    Ok(())
}

pub fn dispatch_cmd(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), crate::error::AppError> {
    match cmd {
        "printgpt" => dispatch_printgpt(da, log_level, app_config)?,
        "r" | "rl" => dispatch_read(da, cmd, args, log_level, app_config)?,
        "w" | "wl" | "e" => dispatch_write(da, cmd, args, verify, log_level, app_config)?,
        "unlock" | "zyb" | "xml" | "frp" | "rpmb" | "genkey" => {
            dispatch_security(da, cmd, args, app_config, log_level)?
        }
        "dumppreloader" | "dumpbrom" => dispatch_dump(da, cmd, args)?,
        "chain" => dispatch_chain(da, args)?,
        "reboot" | "slot" | "adb" | "peek" | "poke" => dispatch_fs_ops(da, cmd, args)?,
        "fs_shell" => dispatch_fs_shell(da, args, app_config, log_level)?,
        _ => {
            let msg = crate::i18n::tr("unknown_command", &[cmd]);
            error!("{}", msg.clone().red());
            super::print_help();
            return Err(crate::error::AppError::Protocol(msg));
        }
    }

    Ok(())
}

// --- Per-family command handlers -------------------------------------------------------------
// Each helper is reached only for the commands routed to it by `dispatch_cmd`, so the inner
// `match` keeps the original per-command behaviour byte-for-byte.

fn dispatch_printgpt(
    da: &mut DAXFlash,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), crate::error::AppError> {
    // Capability-driven entry: plan → execute → verify capabilities, no hard-coded flow.
    // DA was uploaded by handle_command's Phase1, so RealSession directly declares
    // DaLoaded and the Planner only plans "open storage → parse GPT".
    let state = real_device_state(da);
    let mode = crate::core::plan::Mode::parse(mode_label(app_config))
        .unwrap_or(crate::core::plan::Mode::Auto);
    let mut session = crate::session::RealSession::new(da, state, true);
    crate::exec::Executor::new(mode).execute(
        &crate::ops::printgpt::PrintGptOp,
        &mut session,
        true,
        log_level,
    )?;
    Ok(())
}

fn dispatch_read(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), crate::error::AppError> {
    match cmd {
        "r" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or(crate::i18n::t("usage_r_gpt"))?;
                super::partition_table::cmd_read_gpt(da, dir, log_level)?;
            } else if args.iter().any(|a| a == "--dp") {
                // Dynamic super partition reads need super metadata parsing, so use the existing implementation
                super::io::cmd_read(da, args)?;
            } else {
                let part = args.first().ok_or("用法: mtkclient r <part> <file>")?;
                let out = args.get(1).ok_or("用法: mtkclient r <part> <file>")?;
                run_op(
                    da,
                    &crate::ops::read::ReadOp {
                        part: part.clone(),
                        out: out.clone(),
                    },
                    app_config,
                    log_level,
                )?;
            }
        }
        "rl" => {
            let dir = args.first().ok_or(crate::i18n::t("usage_rl"))?;
            super::partition_table::cmd_read_all(da, dir, app_config)?;
        }
        _ => unreachable!("dispatch_cmd routes only r/rl here"),
    }
    Ok(())
}

fn dispatch_write(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), crate::error::AppError> {
    match cmd {
        "wl" => {
            let dir = args.first().ok_or(crate::i18n::t("usage_wl"))?;
            super::partition_table::cmd_write_all(da, dir, verify)?;
        }
        "w" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let file = args.get(1).ok_or(crate::i18n::t("usage_w_gpt"))?;
                super::partition_table::cmd_write_gpt(da, file)?;
            } else {
                let part = args.first().ok_or("用法: mtkclient w <part> <file>")?;
                let file = args.get(1).ok_or("用法: mtkclient w <part> <file>")?;
                run_op(
                    da,
                    &crate::ops::write::WriteOp {
                        part: part.clone(),
                        file: file.clone(),
                        verify,
                    },
                    app_config,
                    log_level,
                )?;
            }
        }
        "e" => {
            let part = args.first().ok_or("用法: mtkclient e <part>")?;
            run_op(
                da,
                &crate::ops::erase::EraseOp {
                    part: part.clone(),
                    check: true,
                },
                app_config,
                log_level,
            )?;
        }
        _ => unreachable!("dispatch_cmd routes only w/wl/e here"),
    }
    Ok(())
}

fn dispatch_security(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    app_config: &AppConfig,
    log_level: u8,
) -> Result<(), crate::error::AppError> {
    match cmd {
        "unlock" => {
            // One-shot unlock: seccfg unlock + disable vbmeta verification (single op, planned once)
            run_op(da, &crate::ops::unlock::UnlockOp, app_config, log_level)?;
        }
        "zyb" => {
            super::handle_zyb_command(da, args, app_config, log_level)?;
        }
        "xml" => {
            super::handle_xml_command(da, args)?;
        }
        "frp" => {
            crate::security::frp::frp_unlock(da)
                .map_err(|e| crate::error::AppError::Security(e.to_string()))?;
        }
        "rpmb" => {
            super::rpmb::cmd_rpmb(da, args)?;
        }
        "genkey" => {
            // RPMB key generation (following mtkclient): a 32-byte random key.
            // Once generated it can be written to the device via the mtkclient-family rpmb flow.
            // Reuse the rpmb module implementation (reading /dev/urandom must use read_exact;
            // std::fs::read loops forever until OOM because the char device never returns EOF)
            let key = crate::da::xflash::protocol::rpmb::generate_key();
            let hex: String = key.iter().map(|b| format!("{b:02X}")).collect();
            println!("{}", "═══ RPMB 密钥（32 字节）═══".cyan().bold());
            println!("  hex : {hex}");
            println!(
                "{}",
                "  ⚠ 密钥仅在本次生成，请立即保存（写入设备后无法再读出）".yellow()
            );
            println!(
                "{}",
                "  烧写流程: 参考 mtkclient rpmb key 写入（DA payload 编程）".dimmed()
            );
        }
        _ => unreachable!("dispatch_cmd routes only security commands here"),
    }
    Ok(())
}

fn dispatch_dump(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
) -> Result<(), crate::error::AppError> {
    match cmd {
        "dumppreloader" => {
            // In a DA-mode session: cmd_dumppreloader reads directly via XFlash read_data (fast and stable);
            // on failure it reports an error instead of silently hinting "run in BROM mode".
            super::dump::cmd_dumppreloader(da, None)?;
        }
        "dumpbrom" => {
            // Extract the BROM itself (0xD1 BROM read path, same chain as dumppreloader)
            let out = args.first().cloned().unwrap_or_else(|| "brom.bin".into());
            // Optional second argument: explicit length (hex or decimal); defaults to the 128 KiB entry point
            let result = match args.get(1) {
                None => da.preloader.dump_brom(),
                Some(v) => {
                    let v = v.trim();
                    let h = crate::util::strip_hex_prefix(v);
                    let n = if h.len() < v.len() {
                        usize::from_str_radix(h, 16)
                    } else {
                        v.parse::<usize>()
                    };
                    let size = n.map_err(|_| {
                        crate::error::AppError::Protocol(format!("无效长度: {}", v))
                    })?;
                    da.preloader.dump_brom_with_size(size)
                }
            };
            match result {
                Ok((data, name)) => {
                    let path = if args.is_empty() { name } else { out };
                    std::fs::write(&path, &data)
                        .map_err(|e| crate::error::AppError::Protocol(format!("写出失败: {e}")))?;
                    info!(
                        "{}",
                        format!("BROM 已提取: {} ({} 字节)", path, data.len()).green()
                    );
                }
                Err(e) => {
                    return Err(crate::error::AppError::Protocol(format!(
                        "BROM 提取失败: {e}"
                    )));
                }
            }
        }
        _ => unreachable!("dispatch_cmd routes only dumppreloader/dumpbrom here"),
    }
    Ok(())
}

fn dispatch_chain(da: &mut DAXFlash, args: &[String]) -> Result<(), crate::error::AppError> {
    // Smart exploit-chain planning: probe device state and print the recommended chain (read-only, not executed)
    let goal = match args.first().map(|s| s.as_str()) {
        Some("read") | Some("printgpt") => crate::exploit::chain::Goal::ReadInfo,
        Some("rp") => crate::exploit::chain::Goal::ReadPartition,
        Some("write") | Some("unlock") | Some("erase") => crate::exploit::chain::Goal::Write,
        Some("peek") | Some("poke") => crate::exploit::chain::Goal::MemoryAccess,
        Some("dump") | Some("dumppreloader") => crate::exploit::chain::Goal::DumpPreloader,
        _ => crate::exploit::chain::Goal::Write,
    };
    let caps = crate::exploit::chain::DeviceCapabilities {
        hw_code: da.preloader.chip.as_ref().map(|c| c.hw_code).unwrap_or(0),
        chip_name: da.preloader.chip.as_ref().map(|c| c.name.to_string()),
        // SLA/DAA state: flags probed during the BROM stage (an exploit chain is needed when security is enabled).
        // The chain command runs inside a DA session, so report conservative values (already bypassed).
        security_enabled: false,
        da_loaded: true, // the chain command runs inside a DA session
        preloader_available: false,
        carbonara_applicable: true,
        preloader_mode: da.preloader.is_preloader_mode,
    };
    let steps = crate::exploit::chain::plan(goal, &caps);
    crate::exploit::chain::log_plan(&steps, goal, &caps);
    Ok(())
}

fn dispatch_fs_ops(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
) -> Result<(), crate::error::AppError> {
    match cmd {
        "reboot" => {
            super::io::cmd_reboot(
                da,
                args,
                !da.preloader.is_preloader_mode,
                da.preloader.is_preloader_mode,
            )?;
        }
        "slot" => {
            super::io::cmd_slot(da, args)?;
        }
        "adb" => {
            da.enable_adb_and_reboot()
                .map_err(|e| crate::error::AppError::Usb(e.to_string()))?;
            info!("{}", crate::i18n::t("adb_rebooting").green());
        }
        "peek" => {
            super::io::cmd_peek(da, args)?;
        }
        "poke" => {
            super::io::cmd_poke(da, args)?;
        }
        _ => unreachable!("dispatch_cmd routes only fs_ops commands here"),
    }
    Ok(())
}

fn dispatch_fs_shell(
    da: &mut DAXFlash,
    args: &[String],
    app_config: &AppConfig,
    log_level: u8,
) -> Result<(), crate::error::AppError> {
    // Extended mode: fs_shell boot [file] / fs_shell proinfo [file]
    // —— besides super, also supports boot ramdisk (cpio) and proinfo NV browsing
    if let Some(mode) = args.first().map(|s| s.as_str()) {
        // Non-interactive actions: fs_shell boot ls|cat|info / fs_shell proinfo show
        let action = match args.get(1).map(|s| s.as_str()) {
            Some("ls") => Some(crate::ops::fsshell::FsAction::List(
                args.get(2).cloned().unwrap_or_default(),
            )),
            Some("cat") => Some(crate::ops::fsshell::FsAction::Cat(
                args.get(2).cloned().unwrap_or_default(),
            )),
            Some("info") => Some(crate::ops::fsshell::FsAction::Info),
            Some("show") => Some(crate::ops::fsshell::FsAction::Show),
            _ => None,
        };
        if let (Some(target), Some(action)) = (
            match mode {
                "boot" => Some(crate::ops::fsshell::FsTarget::Boot),
                "proinfo" => Some(crate::ops::fsshell::FsTarget::Proinfo),
                _ => None,
            },
            action,
        ) {
            // Third form: fs_shell <target> <action> [file]
            let file = args
                .get(2)
                .filter(|a| a.ends_with(".img") || a.ends_with(".bin"))
                .cloned();
            run_op(
                da,
                &crate::ops::fsshell::FsShellOp {
                    target,
                    file,
                    action,
                },
                app_config,
                log_level,
            )?;
            return Ok(());
        }
        if mode == "boot" || mode == "proinfo" {
            super::io::cmd_imgshell(da, mode, &args[1..])?;
            return Ok(());
        }
    }
    // Default: read the GPT to get the super partition address
    da.read_gpt().map_err(|e| {
        crate::error::AppError::Protocol(crate::i18n::tr("read_gpt_failed", &[&e.to_string()]))
    })?;
    let gpt_data = da.last_gpt_data.as_ref().ok_or_else(|| {
        crate::error::AppError::Protocol(crate::i18n::t("gpt_unavailable").into())
    })?;
    let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data).map_err(|e| {
        crate::error::AppError::Parse(crate::i18n::tr("parse_gpt_failed", &[&e.to_string()]))
    })?;

    let super_entry = gpt_info
        .find_partition("super")
        .or_else(|| gpt_info.find_partition("super_b"))
        .ok_or_else(|| {
            crate::error::AppError::Protocol(crate::i18n::t("no_super_in_gpt").into())
        })?;
    let super_addr = super_entry.start_addr;

    info!("super 分区地址: 0x{:08X}", super_addr);

    // Build the read_fn closure
    let da_ref = &mut *da;
    let mut read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
        da_ref.readflash_data_ex(super_addr + offset, len, 8)
    };

    crate::partition::explorer::run_explorer(&mut read_fn)?;
    Ok(())
}

/// Extract the state needed for planning from the connected device.
///
/// Security flags were handled in Phase1 (bypassed if needed), so report them as "resolved" here;
/// when the protocol version cannot be read from the DA file, `DeviceState::is_v6` infers it from the da_code list.
fn real_device_state(da: &DAXFlash) -> crate::core::device::DeviceState {
    let chip = da.preloader.chip.as_ref();
    crate::core::device::DeviceState {
        hw_code: chip.map(|c| c.hw_code).unwrap_or(0),
        da_code: chip.map(|c| c.da_code).unwrap_or(0),
        name: chip.map(|c| c.name.to_string()).unwrap_or_default(),
        protocol: crate::core::device::ProtocolVersion::Unknown,
        safety: crate::core::safety::SafetyFlags::NONE,
        brom_auth_required: false,
        preloader_patched: false,
        da_signature_failed: false,
        current_slot: da.active_slot,
        // CVE-2022-20069 cannot be determined out of thin air on real hardware (needs preloader version/build fingerprint comparison),
        // so default to "not affected": better to plan one path fewer than to run an exploit on a guess.
        // When the device is known to be affected, specify `--exploit cve-20069` explicitly (the apply_key path).
        cve_20069_affected: false,
        cve_20069_patched: false,
    }
}

/// `AppConfig.work_mode` → planning mode label.
fn mode_label(cfg: &AppConfig) -> &'static str {
    match cfg.work_mode {
        crate::system::config::WorkMode::Brom => "brom",
        crate::system::config::WorkMode::Preloader => "preloader",
        crate::system::config::WorkMode::Auto => "auto",
    }
}

/// Execute a single DA command (no Phase1/Phase2 handling)
///
/// Thin wrapper: delegates actual dispatch to `dispatch_cmd`.
pub fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    dispatch_cmd(da, cmd, args, verify, log_level, app_config)?;
    Ok(())
}
