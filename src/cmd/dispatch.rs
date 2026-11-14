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
            super::partition_table::cmd_printgpt(da, log_level);
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
            } else {
                super::io::cmd_read(da, args)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
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
                super::io::cmd_write(da, args, verify)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            }
        }
        "e" => {
            super::io::cmd_erase(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "zyb" => {
            super::handle_zyb_command(da, args)
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
        "chain" => {
            // 智能利用链规划：探测设备状态并打印推荐执行链（只读，不执行）
            let goal = match args.first().map(|s| s.as_str()) {
                Some("read") | Some("printgpt") => crate::exploit::chain::Goal::ReadInfo,
                Some("rp") => crate::exploit::chain::Goal::ReadPartition,
                Some("write") | Some("unlock") | Some("erase") => {
                    crate::exploit::chain::Goal::Write
                }
                Some("peek") | Some("poke") => crate::exploit::chain::Goal::MemoryAccess,
                Some("dump") | Some("dumppreloader") => {
                    crate::exploit::chain::Goal::DumpPreloader
                }
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
            crate::exploit::chain::log_plan(&steps, goal);
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
                if mode == "boot" || mode == "proinfo" {
                    super::io::cmd_imgshell(da, mode, &args[1..])
                        .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
                    return Ok(());
                }
            }
            // 默认：读 GPT 获取 super 分区地址
            da.read_gpt()
                .map_err(|e| crate::error::AppError::Protocol(crate::i18n::tr("read_gpt_failed", &[&e.to_string()])))?;
            let gpt_data = da
                .last_gpt_data
                .as_ref()
                .ok_or_else(|| crate::error::AppError::Protocol(crate::i18n::t("gpt_unavailable").into()))?;
            let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data)
                .map_err(|e| crate::error::AppError::Parse(crate::i18n::tr("parse_gpt_failed", &[&e.to_string()])))?;

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
        }    }

    Ok(())
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
