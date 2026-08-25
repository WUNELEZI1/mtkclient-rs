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
            crate::security::vbmeta::vbmeta_disable(da, mode)
                .map_err(|e| format!("修补失败: {}", e))?;
            info!("{}", "vbmeta 已修补".green());
        }
        "seccfg" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb seccfg unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    da.unlock_bootloader()
                        .map_err(|e| format!("解锁失败: {}", e))?;
                    info!("{}", "Bootloader 已解锁".green());
                }
                "lock" => {
                    da.lock_bootloader()
                        .map_err(|e| format!("锁定失败: {}", e))?;
                    info!("{}", "Bootloader 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb seccfg unlock/lock".into()),
            }
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
