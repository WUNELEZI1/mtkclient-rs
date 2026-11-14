//! `rpmb` 命令：RPMB 密钥烧写与分区读写
//!
//! 用法：
//! ```text
//! rpmb info                              显示 RPMB 容量与会话可用性
//! rpmb init [--rpmbkey <hex>]            初始化 RPMB 会话（可选先烧写密钥）
//! rpmb set-key <hex|random> --yes        烧写 32 字节 RPMB 密钥（不可逆）
//! rpmb read <file> [--sector N] [--sectors N]
//! rpmb write <file> [--sector N] --yes
//! ```
//!
//! 说明：RPMB 密钥一旦写入**无法读出**，也无法在不擦除 RPMB 分区的前提下更换，
//! 因此 `set-key` / `write` 强制要求显式 `--yes` 才会真正下发命令。

use crate::color::Colorize;
use log::info;

use crate::da::DAXFlash;
use crate::da::xflash::protocol::rpmb as proto;

/// 解析形如 `--key value` 的选项，返回 (值, 剩余参数)
fn take_opt<'a>(args: &'a [String], name: &str) -> Option<(&'a str, Vec<String>)> {
    let mut out = Vec::with_capacity(args.len());
    let mut found = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                found = Some(v.as_str());
                i += 2;
                continue;
            }
        } else if let Some(v) = args[i].strip_prefix(&format!("{name}=")) {
            found = Some(v);
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    found.map(|v| (v, out))
}

fn parse_u32(v: &str, what: &str) -> Result<u32, String> {
    let s = v.trim();
    let parsed = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16)
    } else {
        s.parse::<u32>()
    };
    parsed.map_err(|_| format!("{} 不是合法整数: {}", what, v))
}

/// 处理 `rpmb` 子命令
pub fn cmd_rpmb(da: &mut DAXFlash, args: &[String]) -> Result<(), String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("info");
    let rest = &args[1.min(args.len())..];

    match sub {
        "info" => cmd_info(da),
        "init" => cmd_init(da, rest),
        "set-key" | "setkey" => cmd_set_key(da, rest),
        "read" | "r" => cmd_read(da, rest),
        "write" | "w" => cmd_write(da, rest),
        other => Err(format!(
            "未知 rpmb 子命令: {}（可用: info/init/set-key/read/write）",
            other
        )),
    }
}

fn cmd_info(da: &mut DAXFlash) -> Result<(), String> {
    let size = da.rpmb_size().unwrap_or(0);
    println!("{}", "═══ RPMB 信息 ═══".bold());
    println!("  容量        : {}", proto::fmt_rpmb_size(size));
    println!("  帧大小      : {} 字节", proto::RPMB_FRAME_LEN);
    if size > 0 {
        println!("  扇区数      : {}", size / proto::RPMB_FRAME_LEN as u64);
    }
    println!(
        "  DA 扩展     : {}",
        if da.daext {
            "已启用".green()
        } else {
            "未启用（RPMB 命令不可用）".red()
        }
    );
    println!();
    Ok(())
}

fn cmd_init(da: &mut DAXFlash, args: &[String]) -> Result<(), String> {
    let key = take_opt(args, "--rpmbkey").map(|(v, _)| v);
    let parsed = match key {
        Some(v) => {
            let k = proto::parse_key_hex(v)?;
            println!("{}", "  ⚠ 将先烧写指定的 RPMB 密钥，此操作不可逆".yellow());
            Some(k)
        }
        None => None,
    };
    let derived = da.custom_rpmb_init(parsed.as_ref().map(|k| k.as_slice()))?;
    match derived {
        Some(k) => println!("  {} {}", "派生密钥:".dimmed(), proto_hex(&k)),
        None => println!("  {}", "RPMB 会话已初始化".green()),
    }
    Ok(())
}

fn cmd_set_key(da: &mut DAXFlash, args: &[String]) -> Result<(), String> {
    let positional: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let confirmed = positional.iter().any(|s| *s == "--yes" || *s == "-y");
    let spec = positional
        .iter()
        .find(|s| !s.starts_with("--"))
        .copied()
        .ok_or("用法: rpmb set-key <32字节hex|random> --yes")?;

    let key = if spec.eq_ignore_ascii_case("random") {
        let k = proto::generate_key();
        println!("  {} {}", "已生成随机密钥:".dimmed(), proto_hex(&k));
        println!(
            "{}",
            "  请立即抄录保存，写入设备后将无法再次读出".yellow().bold()
        );
        k
    } else {
        proto::parse_key_hex(spec)?
    };

    if !confirmed {
        println!(
            "{}",
            "⚠ RPMB 密钥写入不可逆：确认无误后追加 --yes 重新执行"
                .yellow()
                .bold()
        );
        println!("  待写入密钥: {}", proto_hex(&key));
        return Ok(());
    }

    da.custom_rpmb_set_key(&key)?;
    println!("{}", "RPMB 密钥已写入并通过回显校验".green().bold());
    Ok(())
}

fn cmd_read(da: &mut DAXFlash, args: &[String]) -> Result<(), String> {
    let (sector, args) = match take_opt(args, "--sector") {
        Some((v, a)) => (parse_u32(v, "--sector")?, a),
        None => (0, args.to_vec()),
    };
    let (sectors, args) = match take_opt(&args, "--sectors") {
        Some((v, a)) => (Some(parse_u32(v, "--sectors")?), a),
        None => (None, args),
    };
    let file = args
        .iter()
        .find(|s| !s.starts_with("--"))
        .ok_or("用法: rpmb read <file> [--sector N] [--sectors N]")?;

    let sectors = match sectors {
        Some(n) => n,
        None => {
            // 未指定则整块读取
            let size = da.rpmb_size().unwrap_or(0);
            if size == 0 {
                return Err("无法获取 RPMB 容量，请用 --sectors 指定扇区数".to_string());
            }
            (size / proto::RPMB_FRAME_LEN as u64).min(u32::MAX as u64) as u32
        }
    };
    info!(
        "读取 RPMB: sector=0x{:X}, sectors={} ({})",
        sector,
        sectors,
        proto::fmt_rpmb_size(sectors as u64 * 0x100)
    );
    let data = da.custom_rpmb_read(sector, sectors)?;
    std::fs::write(file, &data).map_err(|e| format!("写入 {} 失败: {}", file, e))?;
    println!(
        "{}",
        format!("RPMB 已读取到 {}（{} 字节）", file, data.len()).green()
    );
    Ok(())
}

fn cmd_write(da: &mut DAXFlash, args: &[String]) -> Result<(), String> {
    let (sector, args) = match take_opt(args, "--sector") {
        Some((v, a)) => (parse_u32(v, "--sector")?, a),
        None => (0, args.to_vec()),
    };
    let confirmed = args.iter().any(|s| s == "--yes" || s == "-y");
    let file = args
        .iter()
        .find(|s| !s.starts_with("--"))
        .ok_or("用法: rpmb write <file> [--sector N] --yes")?;

    let mut data = std::fs::read(file).map_err(|e| format!("读取 {} 失败: {}", file, e))?;
    if !data.len().is_multiple_of(proto::RPMB_FRAME_LEN) {
        let pad = proto::RPMB_FRAME_LEN - data.len() % proto::RPMB_FRAME_LEN;
        println!(
            "{}",
            format!("文件长度不是 0x100 整数倍，末尾补 0x{:X} 字节 0x00", pad).yellow()
        );
        data.resize(data.len() + pad, 0);
    }

    if !confirmed {
        println!(
            "{}",
            format!(
                "⚠ 即将写入 {} 字节到 RPMB sector 0x{:X}，确认后追加 --yes",
                data.len(),
                sector
            )
            .yellow()
            .bold()
        );
        return Ok(());
    }

    da.custom_rpmb_write(sector, &data)?;
    println!(
        "{}",
        format!("RPMB 写入完成（{} 字节）", data.len()).green()
    );
    Ok(())
}

fn proto_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_opt_supports_both_forms() {
        let args: Vec<String> = ["f.bin", "--sector", "0x10"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (v, rest) = take_opt(&args, "--sector").unwrap();
        assert_eq!(v, "0x10");
        assert_eq!(rest, vec!["f.bin".to_string()]);

        let args2: Vec<String> = ["--sectors=8", "f.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (v2, rest2) = take_opt(&args2, "--sectors").unwrap();
        assert_eq!(v2, "8");
        assert_eq!(rest2, vec!["f.bin".to_string()]);
    }

    #[test]
    fn parse_u32_hex_and_dec() {
        assert_eq!(parse_u32("0x20", "s").unwrap(), 32);
        assert_eq!(parse_u32("32", "s").unwrap(), 32);
        assert!(parse_u32("zz", "s").is_err());
    }
}
