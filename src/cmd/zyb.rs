//! zyb 子命令处理（handle_zyb_command）
//!
//! 处理 `zyb <subcmd>` 下的所有子操作（seccfg / oem / vbmeta / erase_data / get_build_prop）。

use crate::color::Colorize;
use log::info;

use crate::da::DAXFlash;

/// 处理 zyb 子命令
pub(crate) fn handle_zyb_command(
    da: &mut DAXFlash,
    args: &[String],
    app_config: &crate::system::config::AppConfig,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient zyb <subcmd> [args]".into());
    }

    let subcmd = args[0].as_str();
    let sub_args = &args[1..];

    match subcmd {
        "vbmeta" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb vbmeta <mode> (0/1/2/3)".into());
            }
            let mode = sub_args[0].parse::<u32>().map_err(|_| "无效模式")?;
            // 能力驱动路径（槽位感知 + 不可判定即中止），与 `--sim vbmeta` 同一份实现
            super::dispatch::run_op(
                da,
                &crate::ops::vbmeta::VbmetaOp { mode },
                app_config,
                log_level,
            )?;
        }
        "seccfg" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb seccfg unlock|lock|info [文件]".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    super::dispatch::run_op(
                        da,
                        &crate::ops::seccfg::SeccfgOp {
                            action: crate::ops::seccfg::SeccfgAction::Unlock,
                        },
                        app_config,
                        log_level,
                    )?;
                    info!("{}", "Bootloader 已解锁".green());
                }
                "lock" => {
                    super::dispatch::run_op(
                        da,
                        &crate::ops::seccfg::SeccfgOp {
                            action: crate::ops::seccfg::SeccfgAction::Lock,
                        },
                        app_config,
                        log_level,
                    )?;
                    info!("{}", "Bootloader 已锁定".green());
                }
                "info" => {
                    match sub_args.get(1) {
                        // 提供文件则离线解析，不连接设备
                        Some(path) => crate::security::seccfg::seccfg_info_file(path)?,
                        None => crate::security::seccfg::seccfg_info(da)
                            .map_err(|e| format!("seccfg 分析失败: {}", e))?,
                    }
                }
                _ => return Err("用法: mtkclient zyb seccfg unlock|lock|info [文件]".into()),
            }
        }
        "format" => {
            // 显式格式化指定分区为 f2fs / ext4（能力驱动，需危险确认）
            if sub_args.len() < 2 {
                return Err("用法: mtkclient zyb format <partition> <f2fs|ext4>".into());
            }
            let part = sub_args[0].clone();
            let fs = sub_args[1].clone();
            super::dispatch::run_op(
                da,
                &crate::ops::format::FormatOp { part, fs },
                app_config,
                log_level,
            )?;
        }
        "oem" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb oem unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    crate::security::frp::frp_unlock(da)
                        .map_err(|e| format!("FRP OEM 解锁失败: {}", e))?;
                    info!("{}", "FRP OEM 已解锁".green());
                }
                "lock" => {
                    crate::security::frp::frp_lock(da)
                        .map_err(|e| format!("FRP OEM 锁定失败: {}", e))?;
                    info!("{}", "FRP OEM 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb oem unlock/lock".into()),
            }
        }
        "get_build_prop" => {
            super::buildprop::cmd_buildprop(da, sub_args)?;
        }
        "erase_data" => {
            super::io::cmd_erase_data(da)?;
        }
        _ => return Err(format!("未知 zyb 子命令: {}", subcmd).into()),
    }

    Ok(())
}
