//! MTK proinfo 分区解析
//!
//! 注意：MTK proinfo 没有全局统一格式，不同机型/代际差异很大。
//! 这里实现两套策略：
//!   1) 若分区起始存在 ASCII magic "PROINFO"，按 `version(u32) + count(u32)`
//!      + 若干 `{name[32], offset(u32), length(u32)}` 块表解析；
//!   2) 否则做启发式扫描：在分区内寻找"32 字节可打印 name + 合理 offset/length"
//!      的候选块表项。
//! 真实设备上若 magic/布局不同，需要按机型校准。
//!
//! 本模块为离线命令，不需要连接设备。

use crate::color::Colorize;

#[derive(Debug, Clone)]
pub struct ProInfoBlock {
    pub name: String,
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug)]
pub struct ProInfo {
    pub magic: Option<String>,
    pub version: Option<u32>,
    pub blocks: Vec<ProInfoBlock>,
    pub scan: Vec<ProInfoBlock>,
}

pub fn parse(buf: &[u8]) -> ProInfo {
    let mut info = ProInfo {
        magic: None,
        version: None,
        blocks: Vec::new(),
        scan: Vec::new(),
    };

    if buf.len() >= 16 && &buf[0..7] == b"PROINFO" {
        info.magic = Some("PROINFO".into());
        info.version = Some(u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]));
        let count = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
        let mut p = 16;
        for _ in 0..count.min(128) {
            if p + 40 > buf.len() {
                break;
            }
            let name = String::from_utf8_lossy(&buf[p..p + 32])
                .trim_end_matches('\0')
                .to_string();
            let offset = u32::from_le_bytes([buf[p + 32], buf[p + 33], buf[p + 34], buf[p + 35]]);
            let length = u32::from_le_bytes([buf[p + 36], buf[p + 37], buf[p + 38], buf[p + 39]]);
            if offset as usize <= buf.len() && length < 0x1_0000 && length > 0 {
                info.blocks.push(ProInfoBlock {
                    name,
                    offset,
                    length,
                });
            }
            p += 40;
        }
    } else {
        // 启发式扫描：以 4 字节为步长，找 32 字节可打印 name + 合理 offset/length
        let mut p = 0;
        while p + 40 <= buf.len() {
            let nb = &buf[p..p + 32];
            if nb.iter().all(|&b| b == 0 || b.is_ascii_graphic() || b == b' ')
                && !nb.iter().all(|&b| b == 0)
            {
                let offset = u32::from_le_bytes([buf[p + 32], buf[p + 33], buf[p + 34], buf[p + 35]]);
                let length = u32::from_le_bytes([buf[p + 36], buf[p + 37], buf[p + 38], buf[p + 39]]);
                if offset % 4 == 0
                    && offset as usize <= buf.len()
                    && length < 0x4000
                    && length > 0
                    && offset != 0
                {
                    info.scan.push(ProInfoBlock {
                        name: String::from_utf8_lossy(nb).trim_end_matches('\0').to_string(),
                        offset,
                        length,
                    });
                }
            }
            p += 4;
        }
    }
    info
}

/// proinfo <proinfo.bin>
/// 解析 MTK proinfo 分区。
pub fn cmd_proinfo(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let file = args
        .first()
        .ok_or("缺少 proinfo.bin 路径（用法: proinfo <proinfo.bin>）")?;
    let buf = std::fs::read(file).map_err(|e| format!("读取 {} 失败: {}", file, e))?;
    let info = parse(&buf);

    println!("{}", format!("==== proinfo 解析 ({} 字节) ====", buf.len()).bold());
    if let Some(m) = &info.magic {
        println!("  magic  : {}", m);
    }
    if let Some(v) = info.version {
        println!("  version: {}", v);
    }
    if !info.blocks.is_empty() {
        println!("  块表(有 magic):");
        for b in &info.blocks {
            println!("    {:<20} offset={:#x} length={}", b.name, b.offset, b.length);
        }
    } else {
        println!("  未识别到 PROINFO magic；启发式扫描候选:");
        for b in &info.scan {
            println!("    {:<20} offset={:#x} length={}", b.name, b.offset, b.length);
        }
        if info.scan.is_empty() {
            println!("    (无候选，可能布局与假定格式不同)");
        }
    }
    Ok(())
}
