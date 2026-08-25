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
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <dir>")?;
                super::partition_table::cmd_read_gpt(da, dir, log_level)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            } else {
                super::io::cmd_read(da, args)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            }
        }
        "rl" => {
            let dir = args.first().ok_or("用法: mtkclient rl <dir>")?;
            super::partition_table::cmd_read_all(da, dir, app_config)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "wl" => {
            let dir = args.first().ok_or("用法: mtkclient wl <dir>")?;
            super::partition_table::cmd_write_all(da, dir, verify)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "w" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let file = args.get(1).ok_or("用法: mtkclient w gpt <文件>")?;
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
        "slot" => {
            super::io::cmd_slot(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "adb" => {
            da.enable_adb_and_reboot()
                .map_err(|e| crate::error::AppError::Usb(e.to_string()))?;
            info!("{}", "ADB 已启用，设备正在重启进入系统".green());
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
            // 读 GPT 获取 super 分区地址
            da.read_gpt()
                .map_err(|e| crate::error::AppError::Protocol(format!("读取 GPT 失败: {}", e)))?;
            let gpt_data = da
                .last_gpt_data
                .as_ref()
                .ok_or_else(|| crate::error::AppError::Protocol("GPT 数据不可用".into()))?;
            let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data)
                .map_err(|e| crate::error::AppError::Parse(format!("解析 GPT 失败: {}", e)))?;

            let super_entry = gpt_info
                .find_partition("super")
                .or_else(|| gpt_info.find_partition("super_b"))
                .ok_or_else(|| {
                    crate::error::AppError::Protocol("GPT 中未找到 super 分区".into())
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
            error!("{}", format!("未知命令: {}", cmd).red());
            super::print_help();
            return Err(crate::error::AppError::Protocol(format!(
                "未知命令: {}",
                cmd
            )));
        }
    }

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
