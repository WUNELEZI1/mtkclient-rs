//! `xml` 子命令 — XML (V6) DA 协议操作（新平台 MT6789+）
//!
//! 与 XFlash 路径并列，供新平台设备显式使用：
//!
//! ```text
//! xml init [主机标识]                 上报主机信息 + 运行参数，初始化 XML 会话
//! xml hw-info                        读取存储/硬件信息
//! xml prop <key>                     读取设备系统属性
//! xml efuse-read                     读取 efuse
//! xml efuse-write <hex>              写入 efuse（hex 字节串）
//! xml boot <at_addr> <jmp_addr>      BOOT-TO 跳转执行
//! xml flash-update                   进入 scatter 刷写流程
//! xml security dev-fw-info           读取固件安全信息
//! xml security flash-policy <file>   下发 flash policy（本地文件）
//! xml security allinone-sig <file>   下发 all-in-one 签名（本地文件）
//! xml r <part> <file>                READ-PARTITION 整分区回读
//! xml w <part> <file>                WRITE-PARTITION 整分区写入
//! xml e <part>                       ERASE-PARTITION 格式化分区
//! xml rb <section> <off> <size> <file>   READ-FLASH 按地址读取
//! xml wb <section> <off> <size> <file>   WRITE-FLASH 按地址写入
//! xml eb <section> <off> <size>          ERASE-FLASH 按地址擦除
//! xml reboot [disconnect]            REBOOT（disconnect=断开重启）
//! xml boot-mode <mode>               SET-BOOT-MODE（如 FASTBOOT / META）
//! ```

use std::fs::File;
use std::io::{BufWriter, Read, Write};

use log::info;

use crate::color::Colorize;
use crate::da::DAXFlash;
use crate::da::xml::XmlSession;

/// 解析地址/长度（支持 `0x` 前缀的十六进制或十进制）
fn parse_num(s: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let s = s.trim();
    let v = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)?
    } else {
        s.parse::<u64>()?
    };
    Ok(v)
}

/// 读取本地文件为字节串（用于 efuse / security 下发）
fn read_input_file(path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut f = File::open(path).map_err(|e| format!("打开文件 {} 失败: {}", path, e))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// 百分位进度打印（每 10% 一行，避免刷屏）
fn progress_logger() -> impl FnMut(usize, usize) {
    let mut last = usize::MAX;
    move |done: usize, total: usize| {
        if total == 0 {
            return;
        }
        let pct = done * 100 / total;
        if pct / 10 != last / 10 {
            last = pct;
            info!("  进度: {}% ({}/{})", pct, done, total);
        }
    }
}

/// `xml` 子命令入口
pub fn handle_xml_command(
    da: &mut DAXFlash,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let sub = args
        .first()
        .map(|s| s.as_str())
        .ok_or("用法: mtkclient xml <init|hw-info|prop|efuse-read|efuse-write|boot|flash-update|security|r|w|e|rb|wb|eb|reboot|boot-mode> ...")?;

    // --via xml 的读/写/擦命令（r/w/e）也复用本会话，故统一在此构建
    let mut session = XmlSession::new(&mut *da.preloader.device);

    match sub {
        "init" => {
            let host = args.get(1).map(|s| s.as_str()).unwrap_or("mtkclient-rs");
            session
                .initialize(host, "INFO", "USB", system_os())
                .map_err(|e| format!("XML 初始化失败: {}", e))?;
            info!("{}", "XML DA 会话已初始化".green());
        }
        "hw-info" => {
            let text = session
                .get_hw_info()
                .map_err(|e| format!("读取硬件信息失败: {}", e))?;
            info!("{}", "硬件信息:".bold());
            println!("{}", text);
        }
        "prop" => {
            let key = args.get(1).ok_or("用法: xml prop <key>")?;
            let text = session
                .get_sys_property(key)
                .map_err(|e| format!("读取属性 {} 失败: {}", key, e))?;
            println!("{}", text);
        }
        "efuse-read" => {
            let text = session
                .read_efuse()
                .map_err(|e| format!("读取 efuse 失败: {}", e))?;
            println!("{}", text);
        }
        "efuse-write" => {
            let hex = args.get(1).ok_or("用法: xml efuse-write <hex>")?;
            let data = crate::util::parse_hex(hex).map_err(|e| format!("hex 解析失败: {}", e))?;
            let n = session
                .write_efuse(&data)
                .map_err(|e| format!("写入 efuse 失败: {}", e))?;
            info!("{}", format!("已写入 efuse {} 字节", n).green());
        }
        "boot" => {
            let at = parse_num(args.get(1).ok_or("用法: xml boot <at_addr> <jmp_addr>")?)?;
            let jmp = parse_num(args.get(2).ok_or("用法: xml boot <at_addr> <jmp_addr>")?)?;
            session
                .boot_to(at, jmp)
                .map_err(|e| format!("BOOT-TO 失败: {}", e))?;
            info!(
                "{}",
                format!("已跳转执行: at=0x{:X} jmp=0x{:X}", at, jmp).green()
            );
        }
        "flash-update" => {
            session
                .flash_update()
                .map_err(|e| format!("FLASH-UPDATE 失败: {}", e))?;
            info!("{}", "已进入 scatter 刷写流程".green());
        }
        "security" => match args.get(1).map(|s| s.as_str()) {
            Some("dev-fw-info") => {
                let text = session
                    .security_get_dev_fw_info()
                    .map_err(|e| format!("读取固件安全信息失败: {}", e))?;
                println!("{}", text);
            }
            Some("flash-policy") => {
                let file = args
                    .get(2)
                    .ok_or("用法: xml security flash-policy <file>")?;
                let data = read_input_file(file)?;
                let n = session
                    .security_set_flash_policy(&data)
                    .map_err(|e| format!("下发 flash policy 失败: {}", e))?;
                info!("{}", format!("已下发 flash policy {} 字节", n).green());
            }
            Some("allinone-sig") => {
                let file = args
                    .get(2)
                    .ok_or("用法: xml security allinone-sig <file>")?;
                let data = read_input_file(file)?;
                let n = session
                    .security_set_allinone_signature(&data)
                    .map_err(|e| format!("下发签名失败: {}", e))?;
                info!("{}", format!("已下发 all-in-one 签名 {} 字节", n).green());
            }
            _ => {
                return Err(
                    "用法: xml security <dev-fw-info|flash-policy <file>|allinone-sig <file>>"
                        .into(),
                );
            }
        },
        "r" => {
            let part = args.get(1).ok_or("用法: xml r <part> <file>")?;
            let out = args.get(2).ok_or("用法: xml r <part> <file>")?;
            let f = File::create(out).map_err(|e| format!("创建 {} 失败: {}", out, e))?;
            let mut w = BufWriter::new(f);
            let mut progress = progress_logger();
            let n = session
                .read_partition(part, &mut w, &mut progress)
                .map_err(|e| format!("READ-PARTITION 失败: {}", e))?;
            w.flush()?;
            info!("{}", format!("{} -> {} ({} 字节)", part, out, n).green());
        }
        "w" => {
            let part = args.get(1).ok_or("用法: xml w <part> <file>")?;
            let path = args.get(2).ok_or("用法: xml w <part> <file>")?;
            let data = read_input_file(path)?;
            let mut progress = progress_logger();
            let mut reader = &data[..];
            session
                .write_partition(part, data.len(), &mut reader, &mut progress)
                .map_err(|e| format!("WRITE-PARTITION 失败: {}", e))?;
            info!(
                "{}",
                format!("{} -> {} ({} 字节)", path, part, data.len()).green()
            );
        }
        "e" => {
            let part = args.get(1).ok_or("用法: xml e <part>")?;
            let mut progress = progress_logger();
            session
                .format_partition(part, &mut progress)
                .map_err(|e| format!("ERASE-PARTITION 失败: {}", e))?;
            info!("{}", format!("分区 {} 已格式化", part).green());
        }
        "rb" => {
            let section = args
                .get(1)
                .ok_or("用法: xml rb <section> <off> <size> <file>")?;
            let off = parse_num(args.get(2).ok_or("缺少偏移")?)?;
            let size = parse_num(args.get(3).ok_or("缺少长度")?)? as usize;
            let out = args.get(4).ok_or("缺少输出文件")?;
            let f = File::create(out).map_err(|e| format!("创建 {} 失败: {}", out, e))?;
            let mut w = BufWriter::new(f);
            let mut progress = progress_logger();
            let n = session
                .read_flash(section, off, size, &mut w, &mut progress)
                .map_err(|e| format!("READ-FLASH 失败: {}", e))?;
            w.flush()?;
            info!(
                "{}",
                format!("{} @0x{:X} -> {} ({} 字节)", section, off, out, n).green()
            );
        }
        "wb" => {
            let section = args
                .get(1)
                .ok_or("用法: xml wb <section> <off> <size> <file>")?;
            let off = parse_num(args.get(2).ok_or("缺少偏移")?)?;
            let size = parse_num(args.get(3).ok_or("缺少长度")?)? as usize;
            let path = args.get(4).ok_or("缺少输入文件")?;
            let data = read_input_file(path)?;
            let mut progress = progress_logger();
            let mut reader = &data[..];
            let n = session
                .write_flash(section, off, size, &mut reader, &mut progress)
                .map_err(|e| format!("WRITE-FLASH 失败: {}", e))?;
            info!(
                "{}",
                format!("{} -> {} @0x{:X} ({} 字节)", path, section, off, n).green()
            );
        }
        "eb" => {
            let section = args.get(1).ok_or("用法: xml eb <section> <off> <size>")?;
            let off = parse_num(args.get(2).ok_or("缺少偏移")?)?;
            let size = parse_num(args.get(3).ok_or("缺少长度")?)? as usize;
            let mut progress = progress_logger();
            session
                .erase_flash(section, off, size, &mut progress)
                .map_err(|e| format!("ERASE-FLASH 失败: {}", e))?;
            info!("{}", format!("{} @0x{:X} 已擦除", section, off).green());
        }
        "reboot" => {
            let disconnect = matches!(args.get(1).map(|s| s.as_str()), Some("disconnect"));
            session
                .reboot(disconnect)
                .map_err(|e| format!("REBOOT 失败: {}", e))?;
            info!("{}", "设备正在重启".green());
        }
        "boot-mode" => {
            let mode = args.get(1).ok_or("用法: xml boot-mode <mode>")?;
            session
                .set_boot_mode(mode, "USB", "ON", "ON")
                .map_err(|e| format!("SET-BOOT-MODE 失败: {}", e))?;
            info!("{}", format!("下次启动模式已设为 {}", mode).green());
        }
        other => {
            return Err(format!("未知 xml 子命令: {}（见 mtkclient --help）", other).into());
        }
    }

    Ok(())
}

/// 当前主机系统标识（对齐 DA 的 system_os 取值）
fn system_os() -> &'static str {
    if cfg!(windows) {
        "WINDOWS"
    } else if cfg!(target_os = "macos") {
        "MACOS"
    } else {
        "LINUX"
    }
}
