//! boot / recovery 镜像解析与 ramdisk 解包
//!
//! 纯 Rust 实现，无外部二进制依赖。压缩格式自动识别：
//!   gzip (1f 8b) / lz4 legacy (02 21 4c 18) / lz4 frame (04 22 4d 18) /
//!   xz (fd 37 7a 58 5a 00) / 未压缩(pipe)
//!
//! 参考 AOSP bootable/recovery/bootimg.h 的字段布局（支持 v0~v4 头部）。
//! 本模块为离线命令，不需要连接设备。

use crate::color::Colorize;
use std::io::{Cursor, Read};

const BOOT_MAGIC: &[u8; 8] = b"ANDROID!";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V0,
    V1,
    V2,
    V3,
    V4,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
pub struct BootImage {
    pub version: Version,
    pub page_size: u32,
    pub kernel_size: u32,
    pub kernel_load: u32,
    pub ramdisk_size: u32,
    pub ramdisk_load: u32,
    pub second_size: u32,
    pub tags_load: u32,
    pub name: String,
    pub cmdline: String,
    pub recovery_dtbo_size: u32,
    pub recovery_dtbo_offset: u64,
    pub dtb_size: u32,
    pub dtb_offset: u64,
    pub kernel_off: u64,
    pub ramdisk_off: u64,
    pub second_off: u64,
}

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
    ])
}
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}
fn slice<'a>(buf: &'a [u8], off: u64, size: u32) -> Result<&'a [u8], String> {
    let end = off + size as u64;
    if end > buf.len() as u64 {
        return Err(format!("越界: off={} size={} > buflen={}", off, size, buf.len()));
    }
    Ok(&buf[off as usize..end as usize])
}

pub fn parse(buf: &[u8]) -> Result<BootImage, String> {
    if buf.len() < 512 {
        return Err("文件过小，不可能是 boot 镜像".into());
    }
    if &buf[0..8] != BOOT_MAGIC {
        return Err(format!(
            "魔数错误：期望 ANDROID!，实际 {:?}",
            &buf[0..8.min(buf.len())]
        ));
    }
    let page_size = rd_u32(buf, 36);
    if page_size == 0 || (page_size & (page_size - 1)) != 0 {
        return Err(format!("page_size 非法: {}", page_size));
    }
    let hv = rd_u32(buf, 40);
    let version = match hv {
        0 => Version::V0,
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        4 => Version::V4,
        _ => return Err(format!("未知 header_version: {}", hv)),
    };
    let ps = page_size as u64;
    let mut bi = BootImage {
        version,
        page_size,
        kernel_size: rd_u32(buf, 8),
        kernel_load: rd_u32(buf, 12),
        ramdisk_size: rd_u32(buf, 16),
        ramdisk_load: rd_u32(buf, 20),
        second_size: rd_u32(buf, 24),
        tags_load: rd_u32(buf, 32),
        name: cstr(&buf[44..60]),
        cmdline: cstr(&buf[60..572]),
        recovery_dtbo_size: if matches!(version, Version::V1 | Version::V2) {
            rd_u32(buf, 1640)
        } else {
            0
        },
        recovery_dtbo_offset: if matches!(version, Version::V1 | Version::V2) {
            rd_u64(buf, 1644)
        } else {
            0
        },
        dtb_size: if version == Version::V2 {
            rd_u32(buf, 1656)
        } else {
            0
        },
        dtb_offset: if version == Version::V2 {
            rd_u64(buf, 1660)
        } else {
            0
        },
        kernel_off: ps,
        ramdisk_off: 0,
        second_off: 0,
    };
    bi.ramdisk_off = ps + align_up(bi.kernel_size as u64, ps);
    bi.second_off = bi.ramdisk_off + align_up(bi.ramdisk_size as u64, ps);
    Ok(bi)
}

// ── 压缩解压 ──

pub fn decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        return gzip_decompress(data);
    }
    if data.len() >= 4 && &data[0..4] == b"\x02\x21\x4c\x18" {
        return lz4_legacy_decompress(data);
    }
    if data.len() >= 4 && &data[0..4] == b"\x04\x22\x4d\x18" {
        return lz4_frame_decompress(data);
    }
    if data.len() >= 6 && &data[0..6] == b"\xfd\x37\x7a\x58\x5a\x00" {
        return xz_decompress(data);
    }
    Ok(data.to_vec())
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;
    let mut d = GzDecoder::new(Cursor::new(data));
    let mut out = Vec::new();
    d.read_to_end(&mut out)
        .map_err(|e| format!("gzip 解压失败: {}", e))?;
    Ok(out)
}

fn xz_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut Cursor::new(data), &mut out)
        .map_err(|e| format!("xz 解压失败: {}", e))?;
    Ok(out)
}

fn lz4_legacy_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 4;
    let mut out = Vec::new();
    while pos + 4 <= data.len() {
        let csize = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
            as usize;
        pos += 4;
        if csize == 0 {
            break;
        }
        if pos + csize > data.len() {
            return Err("lz4 legacy: 块长度越界".into());
        }
        let block = &data[pos..pos + csize];
        pos += csize;
        out.extend_from_slice(&lz4_block_decompress(block)?);
    }
    Ok(out)
}

fn lz4_frame_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = 4;
    let flg = *data.get(pos).ok_or("lz4 frame: 截断")?;
    pos += 1;
    let _hc = *data.get(pos).ok_or("lz4 frame: 截断")?;
    pos += 1;
    let content_size = (flg & 0x08) != 0;
    let _dict_id = (flg & 0x01) != 0;
    let block_chksum = (flg & 0x10) != 0;
    if content_size {
        pos += 8;
    }
    let mut out = Vec::new();
    loop {
        if pos + 4 > data.len() {
            break;
        }
        let raw = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        pos += 4;
        if raw == 0 {
            break;
        }
        let uncompressed = (raw & 0x8000_0000) != 0;
        let size = (raw & 0x7FFF_FFFF) as usize;
        if pos + size > data.len() {
            return Err("lz4 frame: 块长度越界".into());
        }
        let block = &data[pos..pos + size];
        pos += size;
        if block_chksum {
            pos += 4;
        }
        if uncompressed {
            out.extend_from_slice(block);
        } else {
            out.extend_from_slice(&lz4_block_decompress(block)?);
        }
    }
    Ok(out)
}

pub fn lz4_block_decompress(src: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    let slen = src.len();
    while pos < slen {
        let token = src[pos];
        pos += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            while pos < slen {
                let b = src[pos];
                pos += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        if pos + lit > slen {
            return Err("lz4: literal 越界".into());
        }
        out.extend_from_slice(&src[pos..pos + lit]);
        pos += lit;
        if pos == slen {
            break;
        }
        if pos + 2 > slen {
            return Err("lz4: offset 截断".into());
        }
        let offset = (src[pos] as usize) | ((src[pos + 1] as usize) << 8);
        pos += 2;
        if offset == 0 {
            return Err("lz4: offset 为 0".into());
        }
        let mut mlen = (token & 0x0F) as usize;
        if mlen == 15 {
            while pos < slen {
                let b = src[pos];
                pos += 1;
                mlen += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        mlen += 4;
        let start = match out.len().checked_sub(offset) {
            Some(s) => s,
            None => return Err("lz4: 反向引用越界".into()),
        };
        for i in 0..mlen {
            let b = out[start + i];
            out.push(b);
        }
    }
    Ok(out)
}

// ── cpio (newc / 070702) 解析 ──

#[derive(Debug, Clone)]
pub struct CpioEntry {
    pub name: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub data: Vec<u8>,
}

pub fn parse_cpio(buf: &[u8]) -> Result<Vec<CpioEntry>, String> {
    let mut entries = Vec::new();
    let mut pos = 0;
    loop {
        if pos + 110 > buf.len() {
            break;
        }
        let magic = &buf[pos..pos + 6];
        if magic != b"070701" && magic != b"070702" {
            if buf[pos..pos + 6].iter().all(|&b| b == 0) {
                pos += 1;
                continue;
            }
            return Err(format!("cpio 魔数错误 @{}: {:?}", pos, &buf[pos..pos + 6]));
        }
        let hx = |o: usize| {
            let s = &buf[pos + o..pos + o + 8];
            u32::from_str_radix(std::str::from_utf8(s).unwrap_or("0"), 16).unwrap_or(0)
        };
        let _ino = hx(6);
        let mode = hx(14);
        let uid = hx(22);
        let gid = hx(30);
        let _nlink = hx(38);
        let filesize = hx(54) as usize;
        let namesize = hx(94) as usize;
        let name_pos = pos + 110;
        let name = String::from_utf8_lossy(&buf[name_pos..name_pos + namesize.saturating_sub(1)])
            .into_owned();
        // newc 对齐规则（与 Linux initramfs 解析器一致）：
        // name 字段按 (namesize+3)&!3 对齐，数据字段按 (filesize+3)&!3 对齐
        let namefield_len = (namesize + 3) & !3;
        let data_pos = pos + 110 + namefield_len;
        let data_end = data_pos + ((filesize + 3) & !3);
        if name == "TRAILER!!!" {
            break;
        }
        let data = if filesize > 0 && data_pos + filesize <= buf.len() {
            buf[data_pos..data_pos + filesize].to_vec()
        } else {
            Vec::new()
        };
        entries.push(CpioEntry {
            name,
            mode,
            uid,
            gid,
            data,
        });
        pos = data_end;
    }
    Ok(entries)
}

pub fn classify(entries: &[CpioEntry]) -> &'static str {
    for e in entries {
        let n = e.name.as_str();
        if n == "recovery.fstab"
            || n.ends_with("/recovery.fstab")
            || n == "etc/recovery.fstab"
            || n == "sbin/recovery"
            || n == "init.recovery.rc"
        {
            return "recovery";
        }
    }
    "boot"
}

// ── 命令入口 ──

/// boot <boot.img> [--extract <dir>]
/// 解析 boot/recovery 镜像，抽取 kernel/ramdisk/dtbo/dtb，解压 ramdisk、列出 cpio、判定 boot/recovery。
pub fn cmd_boot(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file: Option<String> = None;
    let mut extract: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--extract" => {
                extract = args.get(i + 1).cloned();
                i += 2;
            }
            other if !other.starts_with('-') && file.is_none() => {
                file = Some(other.to_string());
                i += 1;
            }
            _ => {
                return Err(format!("未知参数: {}（用法: boot <boot.img> [--extract <dir>]）", args[i]).into());
            }
        }
    }
    let file = file.ok_or("缺少 boot.img 路径（用法: boot <boot.img> [--extract <dir>]）")?;

    let buf = std::fs::read(&file).map_err(|e| format!("读取 {} 失败: {}", file, e))?;
    let bi = parse(&buf).map_err(|e| e)?;

    println!("{}", "==== boot 头部 ====".bold());
    println!("  版本        : {}", bi.version);
    println!("  page_size   : {}", bi.page_size);
    println!(
        "  kernel      : off={} size={}",
        bi.kernel_off, bi.kernel_size
    );
    println!(
        "  ramdisk     : off={} size={}",
        bi.ramdisk_off, bi.ramdisk_size
    );
    if bi.recovery_dtbo_size > 0 {
        println!(
            "  recovery_dtbo: off={} size={}",
            bi.recovery_dtbo_offset, bi.recovery_dtbo_size
        );
    }
    if bi.dtb_size > 0 {
        println!("  dtb         : off={} size={}", bi.dtb_offset, bi.dtb_size);
    }
    println!("  name        : {}", bi.name);
    println!("  cmdline     : {}", bi.cmdline);

    // 抽取
    let outdir = if let Some(ref d) = extract {
        std::fs::create_dir_all(d).map_err(|e| format!("创建目录 {} 失败: {}", d, e))?;
        Some(std::path::PathBuf::from(d))
    } else {
        None
    };

    let write_extract = |name: &str, data: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref dir) = outdir {
            let p = dir.join(name);
            std::fs::write(&p, data).map_err(|e| format!("写出 {} 失败: {}", p.display(), e))?;
            println!("  {}", format!("已抽取 -> {}", p.display()).dimmed());
        }
        Ok(())
    };

    let kernel = slice(&buf, bi.kernel_off, bi.kernel_size)?;
    write_extract("kernel.bin", kernel)?;

    let ramdisk_raw = slice(&buf, bi.ramdisk_off, bi.ramdisk_size)?;
    write_extract("ramdisk.raw", ramdisk_raw)?;

    if bi.recovery_dtbo_size > 0 {
        let dtbo = slice(&buf, bi.recovery_dtbo_offset, bi.recovery_dtbo_size)?;
        write_extract("recovery_dtbo.bin", dtbo)?;
    }
    if bi.dtb_size > 0 {
        let dtb = slice(&buf, bi.dtb_offset, bi.dtb_size)?;
        write_extract("dtb.bin", dtb)?;
    }

    if bi.ramdisk_size == 0 {
        println!("{}", "无 ramdisk（跳过解包）".yellow());
        return Ok(());
    }

    println!("{}", "==== ramdisk 解压 ====".bold());
    let ramdisk = decompress(ramdisk_raw).map_err(|e| format!("ramdisk 解压失败: {}", e))?;
    write_extract("ramdisk.cpio", &ramdisk)?;
    let entries = parse_cpio(&ramdisk).map_err(|e| format!("cpio 解析失败: {}", e))?;
    println!("  cpio 条目数: {}", entries.len());
    for e in &entries {
        println!(
            "    {:>5o} {:<24} {} bytes",
            e.mode & 0o7777,
            e.name,
            e.data.len()
        );
    }
    let kind = classify(&entries);
    println!(
        "{}",
        format!("判定: 这是一个 **{}** 镜像", kind).green().bold()
    );
    Ok(())
}
