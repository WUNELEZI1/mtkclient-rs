//! `zyb seccfg info` report output (shared by offline files and live devices)
//!
//! Output contents:
//! - format detection (V3 / V4)
//! - hwtype breakdown (SW / V2 / V3 / V4) and plaintext confidence
//! - lock state (decoded from attr, not just the magic)
//! - imginfo name list and metadata

use std::fmt::Write as _;

use crate::color::Colorize;

use super::plain::LockState;
use super::v3::SecCfgV3;
use super::v4::SecCfgV4;

/// 16-byte V3 info_header
const V3_HEADER: &[u8; 16] = b"AND_SECCFG_v\x00\x00\x00\x00";

fn decode_v4_lock(state: u32) -> (&'static str, &'static str) {
    match state {
        1 => ("已锁定", "locked"),
        3 => ("已解锁", "unlocked"),
        0 => ("未初始化", "uninitialized"),
        _ => ("未知", "unknown"),
    }
}

/// Build the seccfg analysis report text (unit-test friendly). `data` is the whole seccfg partition.
pub fn seccfg_report(data: &[u8]) -> Result<String, String> {
    if data.is_empty() {
        return Err("seccfg 数据为空".to_string());
    }
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", "═══ seccfg 安全配置分析 ═══".bold());

    if data.len() >= 16 && &data[0..16] == V3_HEADER {
        report_v3(data, &mut out)?;
    } else if data.len() >= 28
        && u32::from_le_bytes(data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        report_v4(data, &mut out)?;
    } else {
        let _ = writeln!(out, "  格式: {}", "无法识别（既不是 V3 也不是 V4）".red());
        let _ = writeln!(out, "  首 16 字节: {}", hex(&data[..16.min(data.len())]));
        return Err("无法识别 seccfg 格式".to_string());
    }
    Ok(out)
}

/// Print the seccfg analysis report
pub fn print_seccfg_report(data: &[u8]) -> Result<(), String> {
    print!("{}", seccfg_report(data)?);
    Ok(())
}

fn report_v3(data: &[u8], out: &mut String) -> Result<(), String> {
    let v3 = SecCfgV3::parse(data)?;
    let lock = LockState::from_attr(v3.seccfg_attr);

    let _ = writeln!(out, "  格式      : V3 (AND_SECCFG_v)");
    let _ = writeln!(
        out,
        "  加密类型   : {} (明文可信度 {}/100)",
        v3.hwtype.green().bold(),
        v3.plain.as_ref().map(|p| p.score).unwrap_or(0)
    );
    if let Some(p) = &v3.plain {
        let _ = writeln!(
            out,
            "  明文标记   : {}    明文长度: {} 字节",
            p.marker.label(),
            p.len
        );
        let _ = writeln!(
            out,
            "  SIU 状态   : 0x{:08X}    seccfg_status: 0x{:08X}",
            p.siu_status, p.status
        );
    }
    let lock_text = match lock {
        LockState::Locked => "已锁定".red(),
        LockState::Unlocked => "已解锁".green(),
        LockState::Unknown(_) => "未知".yellow(),
    };
    let _ = writeln!(
        out,
        "  锁状态     : {}    attr=0x{:08X}",
        lock_text, v3.seccfg_attr
    );
    let _ = writeln!(
        out,
        "  头字段     : ver={} size={} enc_offset=0x{:X} enc_len=0x{:X}",
        v3.seccfg_ver, v3.seccfg_size, v3.seccfg_enc_offset, v3.seccfg_enc_len
    );
    let _ = writeln!(
        out,
        "  软件锁计数 : try={} done={}    page_size={} page_count={}",
        v3.sw_sec_lock_try, v3.sw_sec_lock_done, v3.page_size, v3.page_count
    );

    if let Some(p) = &v3.plain {
        let named: Vec<&str> = p
            .imgs
            .iter()
            .filter(|i| !i.name.is_empty())
            .map(|i| i.name.as_str())
            .collect();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  镜像条目   : 共 {} 项，其中 {} 项有名称",
            p.imgs.len(),
            named.len()
        );
        for (idx, img) in p.imgs.iter().enumerate() {
            if img.name.is_empty() {
                continue;
            }
            let meta: Vec<String> = img
                .words
                .iter()
                .take(4)
                .map(|w| format!("0x{:08X}", w))
                .collect();
            let _ = writeln!(
                out,
                "    [{:>2}] {:<24} meta: {}",
                idx,
                img.name,
                meta.join(" ")
            );
        }
    }
    let _ = writeln!(out);
    Ok(())
}

fn report_v4(data: &[u8], out: &mut String) -> Result<(), String> {
    let v4 = SecCfgV4::parse(data)?;
    let (zh, _en) = decode_v4_lock(v4.lock_state);
    let lock_text = if v4.lock_state == 3 {
        zh.green()
    } else if v4.lock_state == 1 {
        zh.red()
    } else {
        zh.yellow()
    };

    let _ = writeln!(out, "  格式      : V4 (28 字节头 + SHA256/AES 尾)");
    let _ = writeln!(out, "  加密类型   : {}", v4.hwtype.green().bold());
    let _ = writeln!(
        out,
        "  锁状态     : {}    lock_state={}",
        lock_text, v4.lock_state
    );
    let _ = writeln!(
        out,
        "  dm-verity  : {}    sboot_runtime=0x{:08X}",
        if v4.dm_verity_state == 0 {
            "正常(0)".green()
        } else {
            "异常/已关闭".yellow()
        },
        v4.sboot_runtime
    );
    let _ = writeln!(out, "  分区大小   : {} 字节", v4.seccfg_size);
    let _ = writeln!(out);
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{:02X}", x))
        .collect::<Vec<_>>()
        .join(" ")
}
