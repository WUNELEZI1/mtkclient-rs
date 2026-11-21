//! 统一命令分发（dispatch_cmd / execute_single_command）
//!
//! `dispatch_cmd` 是所有命令 match 逻辑的唯一来源，被 `execute_single_command`、
//! `multi::cmd_multi`、`script::dispatch_command` 共同调用，确保三处命令列表和逻辑一致。

use crate::color::Colorize;
use log::{error, info};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

/// 统一命令分发函数（所有命令 match 逻辑的唯一来源）
///
/// 被 `execute_single_command`、`multi::cmd_multi`、`script::dispatch_command` 共同调用，
/// 确保三处命令列表和逻辑完全一致。
/// 用能力驱动路径执行一个 op（真机）。
///
/// 与 `printgpt` 走同一条链路：RealSession（DA 已由 Phase1 上传）→ Executor →
/// Orchestrator 决策 → 逐步执行 + 能力校验。确认策略来自 `--yes`（直接放行）或交互式 y/N。
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
        .execute(op, &mut session, true, log_level)
        .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
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
        "printgpt" => {
            // 能力驱动入口：规划 → 执行 → 能力校验，不再硬编码流程。
            // DA 已由 handle_command 的 Phase1 上传完成，因此 RealSession 直接声明
            // DaLoaded，Planner 只会规划 "开存储 → 解析 GPT"。
            let state = real_device_state(da);
            let mode = crate::core::plan::Mode::parse(mode_label(app_config))
                .unwrap_or(crate::core::plan::Mode::Auto);
            let mut session = crate::session::RealSession::new(da, state, true);
            crate::exec::Executor::new(mode)
                .execute(
                    &crate::ops::printgpt::PrintGptOp,
                    &mut session,
                    true,
                    log_level,
                )
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "dumppreloader" => {
            // DA 模式会话下：cmd_dumppreloader 内部走 XFlash read_data 直接读出（快、稳），
            // 失败时明确报错，不再静默忽略提示"请在 BROM 模式下执行"。
            super::dump::cmd_dumppreloader(da, None)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "r" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or(crate::i18n::t("usage_r_gpt"))?;
                super::partition_table::cmd_read_gpt(da, dir, log_level)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            } else if args.iter().any(|a| a == "--dp") {
                // super 动态分区读取需要 super 元数据解析，仍走既有实现
                super::io::cmd_read(da, args)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
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
            super::partition_table::cmd_read_all(da, dir, app_config)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "wl" => {
            let dir = args.first().ok_or(crate::i18n::t("usage_wl"))?;
            super::partition_table::cmd_write_all(da, dir, verify)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "w" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let file = args.get(1).ok_or(crate::i18n::t("usage_w_gpt"))?;
                super::partition_table::cmd_write_gpt(da, file)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
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
        "unlock" => {
            // 一键解锁：seccfg unlock + vbmeta 关校验（同一 op，规划只做一次）
            run_op(da, &crate::ops::unlock::UnlockOp, app_config, log_level)?;
        }
        "zyb" => {
            super::handle_zyb_command(da, args, app_config, log_level)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "xml" => {
            super::handle_xml_command(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "frp" => {
            crate::security::frp::frp_unlock(da)
                .map_err(|e| crate::error::AppError::Security(e.to_string()))?;
        }
        "reboot" => {
            super::io::cmd_reboot(
                da,
                args,
                !da.preloader.is_preloader_mode,
                da.preloader.is_preloader_mode,
            )
            .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "dumpbrom" => {
            // BROM 本体提取（0xD1 BROM 读取路径，与 dumppreloader 同链路）
            let out = args.first().cloned().unwrap_or_else(|| "brom.bin".into());
            // 可选第二个参数：显式长度（十六进制或十进制）；不传则走默认 128 KiB 入口
            let result = match args.get(1) {
                None => da.preloader.dump_brom(),
                Some(v) => {
                    let v = v.trim();
                    let n = if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
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
        "rpmb" => {
            super::rpmb::cmd_rpmb(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "genkey" => {
            // RPMB 密钥生成（参考 mtkclient）：32 字节随机密钥。
            // 生成后可用 mtkclient 家族的 rpmb 烧写流程写入设备。
            // 复用 rpmb 模块的实现（读取 /dev/urandom 必须用 read_exact，
            // std::fs::read 会因字符设备不返回 EOF 而无限读取直至 OOM）
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
        "chain" => {
            // 智能利用链规划：探测设备状态并打印推荐执行链（只读，不执行）
            let goal = match args.first().map(|s| s.as_str()) {
                Some("read") | Some("printgpt") => crate::exploit::chain::Goal::ReadInfo,
                Some("rp") => crate::exploit::chain::Goal::ReadPartition,
                Some("write") | Some("unlock") | Some("erase") => {
                    crate::exploit::chain::Goal::Write
                }
                Some("peek") | Some("poke") => crate::exploit::chain::Goal::MemoryAccess,
                Some("dump") | Some("dumppreloader") => crate::exploit::chain::Goal::DumpPreloader,
                _ => crate::exploit::chain::Goal::Write,
            };
            let caps = crate::exploit::chain::DeviceCapabilities {
                hw_code: da.preloader.chip.as_ref().map(|c| c.hw_code).unwrap_or(0),
                chip_name: da.preloader.chip.as_ref().map(|c| c.name.to_string()),
                // SLA/DAA 状态：BROM 阶段探测标志（安全启用时需要 exploit 链）。
                // chain 命令运行于 DA 会话内，此处按保守值（已绕过阶段）上报。
                security_enabled: false,
                da_loaded: true, // chain 命令在 DA 会话内运行
                preloader_available: false,
                carbonara_applicable: true,
                preloader_mode: da.preloader.is_preloader_mode,
            };
            let steps = crate::exploit::chain::plan(goal, &caps);
            crate::exploit::chain::log_plan(&steps, goal, &caps);
        }
        "slot" => {
            super::io::cmd_slot(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "adb" => {
            da.enable_adb_and_reboot()
                .map_err(|e| crate::error::AppError::Usb(e.to_string()))?;
            info!("{}", crate::i18n::t("adb_rebooting").green());
        }
        "peek" => {
            super::io::cmd_peek(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "poke" => {
            super::io::cmd_poke(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "fs_shell" => {
            // 扩展模式：fs_shell boot [file] / fs_shell proinfo [file]
            // —— 除 super 外还支持 boot ramdisk (cpio) 与 proinfo NV 浏览
            if let Some(mode) = args.first().map(|s| s.as_str()) {
                // 非交互动作：fs_shell boot ls|cat|info / fs_shell proinfo show
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
                    // 第三种形式：fs_shell <target> <action> [file]
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
                    super::io::cmd_imgshell(da, mode, &args[1..])
                        .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
                    return Ok(());
                }
            }
            // 默认：读 GPT 获取 super 分区地址
            da.read_gpt().map_err(|e| {
                crate::error::AppError::Protocol(crate::i18n::tr(
                    "read_gpt_failed",
                    &[&e.to_string()],
                ))
            })?;
            let gpt_data = da.last_gpt_data.as_ref().ok_or_else(|| {
                crate::error::AppError::Protocol(crate::i18n::t("gpt_unavailable").into())
            })?;
            let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data).map_err(|e| {
                crate::error::AppError::Parse(crate::i18n::tr(
                    "parse_gpt_failed",
                    &[&e.to_string()],
                ))
            })?;

            let super_entry = gpt_info
                .find_partition("super")
                .or_else(|| gpt_info.find_partition("super_b"))
                .ok_or_else(|| {
                    crate::error::AppError::Protocol(crate::i18n::t("no_super_in_gpt").into())
                })?;
            let super_addr = super_entry.start_addr;

            info!("super 分区地址: 0x{:08X}", super_addr);

            // 构造 read_fn 闭包
            let da_ref = &mut *da;
            let mut read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
                da_ref.readflash_data_ex(super_addr + offset, len, 8)
            };

            crate::partition::explorer::run_explorer(&mut read_fn)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        _ => {
            let msg = crate::i18n::tr("unknown_command", &[cmd]);
            error!("{}", msg.clone().red());
            super::print_help();
            return Err(crate::error::AppError::Protocol(msg));
        }
    }

    Ok(())
}

/// 从已连接设备提取规划所需状态。
///
/// 安全标志在 Phase1 已完成（必要时已 bypass），因此这里按"已解决"上报；
/// 协议版本无法从 DA 文件读取时交给 `DeviceState::is_v6` 用 da_code 名单预判。
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
        // 真机无法凭空判定 CVE-2022-20069（需要 preloader 版本/构建指纹比对），
        // 默认按"不受影响"处理：宁可少规划一条路径，也不要凭猜测去打 exploit。
        // 确知设备受影响时用 `--exploit cve-20069` 显式指定（走 apply_key 路径）。
        cve_20069_affected: false,
        cve_20069_patched: false,
    }
}

/// `AppConfig.work_mode` → 规划模式标签。
fn mode_label(cfg: &AppConfig) -> &'static str {
    match cfg.work_mode {
        crate::system::config::WorkMode::Brom => "brom",
        crate::system::config::WorkMode::Preloader => "preloader",
        crate::system::config::WorkMode::Auto => "auto",
    }
}

/// 执行单个 DA 命令（不处理 Phase1/Phase2）
///
/// 薄包装：委托给 `dispatch_cmd` 完成实际命令分发。
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
