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
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}
fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}
pub(super) fn slice(buf: &[u8], off: u64, size: u32) -> Result<&[u8], String> {
    let end = off + size as u64;
    if end > buf.len() as u64 {
        return Err(format!(
            "越界: off={} size={} > buflen={}",
            off,
            size,
            buf.len()
        ));
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
        name: cstr(&buf[48..64]),
        cmdline: cstr(&buf[64..576]),
        recovery_dtbo_size: if matches!(version, Version::V1 | Version::V2) {
            rd_u32(buf, 1632)
        } else {
            0
        },
        recovery_dtbo_offset: if matches!(version, Version::V1 | Version::V2) {
            rd_u64(buf, 1636)
        } else {
            0
        },
        dtb_size: if version == Version::V2 {
            rd_u32(buf, 1648)
        } else {
            0
        },
        // dtb_addr at header offset 1652 is a "load address", not a file offset;
        // in v2 the dtb is appended sequentially after the other segments (page-aligned), so set it to 0 here and compute the order below.
        dtb_offset: 0,
        kernel_off: ps,
        ramdisk_off: 0,
        second_off: 0,
    };
    bi.ramdisk_off = ps + align_up(bi.kernel_size as u64, ps);
    bi.second_off = bi.ramdisk_off + align_up(bi.ramdisk_size as u64, ps);
    // v2 dtb file offset: after recovery_dtbo (or right after second/ramdisk if absent), appended page-aligned in order
    bi.dtb_offset = if bi.dtb_size > 0 {
        let base = if bi.recovery_dtbo_size > 0 {
            bi.recovery_dtbo_offset + bi.recovery_dtbo_size as u64
        } else {
            bi.second_off
        };
        align_up(base, ps)
    } else {
        0
    };
    Ok(bi)
}
