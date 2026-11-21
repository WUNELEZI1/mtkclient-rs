//! multi command: run several subcommands in one DA session
//!
//! Mirrors Python mtkclient's multi command: semicolon-separated commands run in order in the same DA session.

use crate::color::Colorize;
use log::{error, info};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

/// Run the multi command
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

        // Parse the subcommand
        let parts: Vec<String> = cmd_str.split_whitespace().map(String::from).collect();
        if parts.is_empty() {
            continue;
        }

        let sub_cmd = parts[0].as_str();
        let sub_args = &parts[1..];

        let result: Result<(), Box<dyn std::error::Error>> = super::dispatch_cmd(
            da,
            sub_cmd,
            sub_args,
            app_config.verify,
            log_level,
            app_config,
        )
        .map_err(|e| e.into());

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
                // A failing subcommand does not abort the remaining ones
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
