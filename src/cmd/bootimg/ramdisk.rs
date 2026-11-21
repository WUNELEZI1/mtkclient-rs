use std::io::{Cursor, Read};

// ── Decompression ──

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
        let csize =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
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
    let _bd = *data.get(pos).ok_or("lz4 frame: 截断")?; // BD: maximum block size
    pos += 1;
    let content_size = (flg & 0x08) != 0;
    let dict_id = (flg & 0x01) != 0;
    let block_chksum = (flg & 0x10) != 0;
    if content_size {
        pos += 8; // 8-byte content_size field
    }
    if dict_id {
        pos += 4; // 4-byte dictID field
    }
    pos += 1; // HC: 1-byte Header Checksum (mandatory in a standard frame; covers FLG/BD/optional fields;
    // skipping this byte previously shifted all following block lengths)
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

// ── cpio (newc / 070702) parsing ──

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
        // newc alignment rules (same as the Linux initramfs parser):
        // the name field is aligned to (namesize+3)&!3, the data field to (filesize+3)&!3
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
