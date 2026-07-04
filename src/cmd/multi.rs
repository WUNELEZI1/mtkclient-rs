//! multi 命令：一次 DA 会话执行多个子命令
//!
//! 对齐 Python mtkclient 的 multi 命令：分号分隔的多个命令在同一 DA 会话中依次执行。

use colored::Colorize;
use log::{error, info};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

/// 执行 multi 命令
pub fn cmd_multi(
    da: &mut DAXFlash,
    args: &[String],
    app_config: &AppConfig,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient multi \"<cmd1>;<cmd2>;...\"".into());
    }

    let commands_str = &args[0];
    let commands: Vec<&str> = commands_str.split(';').collect();

    info!(
        "{}",
        format!("multi 模式: {} 个命令待执行", commands.len())
            .green()
            .bold()
    );
    println!();

    let mut success_count = 0;
    let mut fail_count = 0;

    for (i, cmd_str) in commands.iter().enumerate() {
        let cmd_str = cmd_str.trim();
        if cmd_str.is_empty() {
            continue;
        }

        info!("[{}/{}] 执行: {}", i + 1, commands.len(), cmd_str.cyan());

        // 解析子命令
        let parts: Vec<String> = cmd_str.split_whitespace().map(String::from).collect();
        if parts.is_empty() {
            continue;
        }

        let sub_cmd = parts[0].as_str();
        let sub_args = &parts[1..];

        let result: Result<(), Box<dyn std::error::Error>> = match sub_cmd {
            "printgpt" => {
                super::partition_table::cmd_printgpt(da, log_level);
                Ok(())
            }
            "r" => {
                if sub_args.first().map(|s| s.as_str()) == Some("gpt") {
                    let dir = sub_args.get(1).ok_or("用法: mtkclient r gpt <dir>")?;
                    super::partition_table::cmd_read_gpt(da, dir, log_level)
                } else {
                    super::io::cmd_read(da, sub_args)
                }
            }
            "rl" => {
                let dir = sub_args.first().ok_or("用法: mtkclient rl <dir>")?;
                super::partition_table::cmd_read_all(da, dir, app_config)
            }
            "wl" => {
                let dir = sub_args.first().ok_or("用法: mtkclient wl <dir>")?;
                super::partition_table::cmd_write_all(da, dir, app_config.verify)
            }
            "w" => super::io::cmd_write(da, sub_args, app_config.verify),
            "e" => super::io::cmd_erase(da, sub_args),
            "zyb" => super::execute_single_command(
                da,
                "zyb",
                sub_args,
                app_config.verify,
                log_level,
                app_config,
            ),
            "frp" => crate::security::frp::frp_unlock(da).map_err(|e| e.into()),
            "reboot" => super::io::cmd_reboot(da, sub_args),
            "slot" => super::io::cmd_slot(da, sub_args),
            "adb" => {
                da.enable_adb_and_reboot()
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                info!("{}", "ADB 已启用，设备正在重启进入系统".green());
                Ok(())
            }
            _ => {
                error!("{}", format!("未知命令: {}", sub_cmd).red());
                Err(format!("未知命令: {}", sub_cmd).into())
            }
        };

        match result {
            Ok(()) => {
                success_count += 1;
                info!("{}", format!("[{}/{}] 完成", i + 1, commands.len()).green());
            }
            Err(e) => {
                fail_count += 1;
                error!(
                    "{}",
                    format!("[{}/{}] 失败: {}", i + 1, commands.len(), e).red()
                );
                // multi 中单个命令失败不中断后续命令
            }
        }

        println!();
    }

    info!(
        "multi 执行完毕: {} 成功, {} 失败, 共 {} 个命令",
        success_count,
        fail_count,
        commands.len()
    );

    if fail_count > 0 {
        Err(format!("{} 个命令执行失败", fail_count).into())
    } else {
        Ok(())
    }
}
