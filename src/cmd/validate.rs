//! Command pre-validation (before DA is loaded)

use super::{io, print_help};
use crate::color::Colorize;
use log::error;

/// Validate a command before DA is loaded.
/// Returns Ok(()) when the command format is valid, Err when the command is unknown or its arguments are invalid
pub(crate) fn validate_command(
    cmd: &str,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" | "dumppreloader" | "adb" | "fs_shell" | "frp" | "boot" | "proinfo" => {}
        "rpmb" => {
            if args.is_empty() {
                return Err("用法: mtkclient rpmb info|init|set-key|read|write".into());
            }
            match args[0].as_str() {
                "info" | "init" => {}
                "set-key" | "setkey" => {
                    if args.len() < 2 {
                        return Err("用法: rpmb set-key <32字节hex|random> --yes".into());
                    }
                    if args[1] != "random" {
                        // Validate the key up front so format errors surface before DA is loaded
                        crate::da::xflash::protocol::rpmb::parse_key_hex(&args[1])?;
                    }
                }
                "read" | "r" | "write" | "w" => {
                    if args.len() < 2 {
                        return Err("用法: rpmb read|write <file> [--sector N] [--yes]".into());
                    }
                }
                other => {
                    return Err(format!(
                        "未知 rpmb 子命令: {}（可用: info/init/set-key/read/write）",
                        other
                    )
                    .into());
                }
            }
        }
        "peek" => {
            if args.is_empty() {
                return Err("用法: peek <addr> [size]".into());
            }
            // Validate addr and size up front so argument errors surface before DA is loaded
            let _addr = io::parse_addr(&args[0])?;
            if args.len() >= 2 {
                let size = io::parse_size(&args[1])?;
                if size == 0 {
                    return Err("size 不能为 0".into());
                }
                if size > 0x100000 {
                    return Err("size 不能超过 1MB（peek 适用于小范围内存读取）".into());
                }
            }
        }
        "poke" => {
            if args.len() < 2 {
                return Err("用法: poke <addr> <hex_data>".into());
            }
            let _addr = io::parse_addr(&args[0])?;
            let _data =
                crate::util::parse_hex(&args[1]).map_err(|e| format!("hex 数据解析失败: {}", e))?;
        }
        "r" => {
            if args.is_empty() {
                return Err("用法: mtkclient r <分区名> <文件>".into());
            }
        }
        "rl" => {
            if args.is_empty() {
                return Err("用法: mtkclient rl <目录>".into());
            }
        }
        "w" => {
            if args.len() < 2 {
                return Err("用法: mtkclient w <分区名> <文件>".into());
            }
        }
        "wl" => {
            if args.is_empty() {
                return Err("用法: mtkclient wl <目录>".into());
            }
        }
        "e" => {
            if args.is_empty() {
                return Err("用法: mtkclient e <分区名>".into());
            }
        }
        "reboot" => {}
        "slot" => {
            if args.is_empty() {
                return Err("用法: mtkclient slot show|a|b".into());
            }
            // Validate the slot sub-operation up front: on a typo, return usage hints directly
            // without loading DA or resetting the session (as the user expects: a bad command just reports usage)
            match args[0].as_str() {
                "show" | "a" | "b" => {}
                other => {
                    return Err(format!(
                        "用法: mtkclient slot show|a|b（未知槽位操作: {}）",
                        other
                    )
                    .into());
                }
            }
        }
        "zyb" => {
            if args.is_empty() {
                return Err("用法: mtkclient zyb <subcmd> [args]".into());
            }
            match args[0].as_str() {
                "seccfg" => {
                    if args.len() < 2 || !matches!(args[1].as_str(), "unlock" | "lock" | "info") {
                        return Err("用法: mtkclient zyb seccfg unlock|lock|info [文件]".into());
                    }
                }
                "oem" => {
                    if args.len() < 2 || !matches!(args[1].as_str(), "unlock" | "lock") {
                        return Err("用法: mtkclient zyb oem unlock|lock".into());
                    }
                }
                "vbmeta" => {
                    if args.len() < 2 {
                        return Err("用法: mtkclient zyb vbmeta <0-3>".into());
                    }
                }
                "erase_data" | "get_build_prop" => {}
                _ => {
                    return Err(format!(
                        "未知 zyb 子命令: {}（可用: seccfg/oem/vbmeta/erase_data/get_build_prop）",
                        args[0]
                    )
                    .into());
                }
            }
        }
        "xml" => {
            if args.is_empty() {
                return Err(
                    "用法: mtkclient xml <init|hw-info|prop|efuse-read|efuse-write|boot|flash-update|security|r|w|e|rb|wb|eb|reboot|boot-mode> ..."
                        .into(),
                );
            }
        }
        "multi" => {
            if args.is_empty() {
                return Err("用法: mtkclient multi \"<cmd1>;<cmd2>\"".into());
            }
        }
        "run" => {
            if args.is_empty() {
                return Err("用法: mtkclient run <script.json>".into());
            }
        }
        _ => {
            error!("{}", format!("未知命令: {}", cmd).red());
            print_help();
            return Err(format!("未知命令: {}", cmd).into());
        }
    }
    Ok(())
}
