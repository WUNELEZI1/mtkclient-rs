use super::{LP_GEOMETRY_MAGIC, LP_GEOMETRY_MAGIC_ALT, LP_HEADER_MAGIC};

/// LP extent 映射: super.img 中的一段连续物理区域
#[derive(Debug, Clone)]
pub(crate) struct LpExtent {
    pub(crate) phys_offset: u64,
    pub(crate) size: u64,
}

/// LP 逻辑分区
#[derive(Debug, Clone)]
pub(crate) struct LpPartition {
    pub(crate) name: String,
    pub(crate) attributes: u32,
    pub(crate) first_extent_index: u32,
    pub(crate) num_extents: u32,
}

/// LP 翻译器: 将分区逻辑偏移翻译为 super.img 物理偏移
#[derive(Debug, Clone)]
pub(crate) struct LpTranslator {
    pub(crate) extents: Vec<LpExtent>,
    pub(crate) total_size: u64,
}

impl LpTranslator {
    /// 创建 identity translator（逻辑偏移 = 物理偏移）
    /// 用于非 super 内的独立分区（如 system_a 直接读取场景）
    pub(crate) fn identity(partition_size: u64) -> Self {
        LpTranslator {
            extents: vec![LpExtent {
                phys_offset: 0,
                size: partition_size,
            }],
            total_size: partition_size,
        }
    }

    /// 简单翻译，不跨 extent，用于 superblock/BGD/inode 等小块
    pub(crate) fn logical_to_abs(&self, logical_offset: u64) -> Result<u64, String> {
        let mut acc = 0u64;
        for ext in &self.extents {
            if logical_offset < acc + ext.size {
                return Ok(ext.phys_offset + (logical_offset - acc));
            }
            acc += ext.size;
        }
        Err(format!(
            "offset 0x{:X} 超出 LP 范围 (total 0x{:X})",
            logical_offset, self.total_size
        ))
    }

    /// 翻译一段连续的分区逻辑偏移（可能跨 LP extent 边界）
    /// 返回多个 (super_img_offset, length) 片段
    pub(crate) fn translate_range(&self, mut pos: u64, mut remaining: u64) -> Vec<(u64, u64)> {
        let mut result = Vec::new();
        let mut acc = 0u64;
        for ext in &self.extents {
            if remaining == 0 {
                break;
            }
            let ext_end = acc + ext.size;
            if pos < ext_end {
                let start = pos - acc;
                let to_read = remaining.min(ext.size - start);
                result.push((ext.phys_offset + start, to_read));
                pos += to_read;
                remaining -= to_read;
            }
            acc = ext_end;
        }
        result
    }
}

/// 解析 LP 元数据，只读两块 4KB: geometry @ 0x1000, header @ 0x3000
pub(crate) fn read_lp_metadata<F>(read_fn: &mut F) -> Result<(Vec<LpPartition>, Vec<LpExtent>), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // 1. 读取 geometry @ 0x1000
    let geo_data = read_fn(0x1000, 4096)?;
    if geo_data.len() < 52 {
        return Err(format!("短读：期望 52 字节（LP geometry），实际 {}", geo_data.len()));
    }
    let geo_magic = u32::from_le_bytes(geo_data[0..4].try_into().unwrap());
    if geo_magic != LP_GEOMETRY_MAGIC && geo_magic != LP_GEOMETRY_MAGIC_ALT {
        return Err(format!(
            "LP geometry magic 不匹配: 0x{:08X} (期望 0x{:08X} 或 0x{:08X})",
            geo_magic, LP_GEOMETRY_MAGIC, LP_GEOMETRY_MAGIC_ALT
        ));
    }
    let _logical_block_size = u32::from_le_bytes(geo_data[48..52].try_into().unwrap());

    // 2. 读取 header @ 0x3000
    let hdr_data = read_fn(0x3000, 4096)?;
    if hdr_data.len() < 4 {
        return Err(format!("短读：期望 4 字节（LP header magic），实际 {}", hdr_data.len()));
    }
    let hdr_magic = u32::from_le_bytes(hdr_data[0..4].try_into().unwrap());
    if hdr_magic != LP_HEADER_MAGIC {
        // 备选：扫描前 32KB 找 header magic
        let scan_data = read_fn(0, 0x8000)?;
        let mut hdr_offset = None;
        for off in (0..scan_data.len().saturating_sub(256)).step_by(512) {
            let m = u32::from_le_bytes(scan_data[off..off + 4].try_into().unwrap());
            if m == LP_HEADER_MAGIC {
                hdr_offset = Some(off);
                break;
            }
        }
        let ho = hdr_offset.ok_or(format!(
            "LP header magic 不匹配 @ 0x3000: 0x{:08X}, 扫描也未找到 0x{:08X}",
            hdr_magic, LP_HEADER_MAGIC
        ))?;
        // 用扫描到的数据重新解析
        return parse_lp_from_header(&scan_data[ho..], read_fn);
    }

    parse_lp_from_header(&hdr_data, read_fn)
}

/// 从 header 数据中解析 LP metadata
fn parse_lp_from_header<F>(
    hdr_data: &[u8],
    read_fn: &mut F,
) -> Result<(Vec<LpPartition>, Vec<LpExtent>), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    if hdr_data.len() < 104 {
        return Err(format!("短读：期望 104 字节（LP header），实际 {}", hdr_data.len()));
    }
    let header_size = u32::from_le_bytes(hdr_data[8..12].try_into().unwrap());
    let tables_base = 0x3000 + header_size as u64;

    // TableDescriptor: offset(u32) + num_entries(u32) + entry_size(u32)
    let read_td = |off: usize| -> (u32, u32, u32) {
        let d = &hdr_data[off..off + 12];
        (
            u32::from_le_bytes(d[0..4].try_into().unwrap()),
            u32::from_le_bytes(d[4..8].try_into().unwrap()),
            u32::from_le_bytes(d[8..12].try_into().unwrap()),
        )
    };

    let (part_off, part_count, part_entry_size) = read_td(80);
    let (ext_off, ext_count, ext_entry_size) = read_td(92);

    // 读取 partition 表
    let part_bytes = (part_count as u64) * (part_entry_size as u64);
    let part_data = read_fn(tables_base + part_off as u64, part_bytes)?;
    let mut partitions = Vec::with_capacity(part_count as usize);
    for i in 0..part_count {
        let eo = (i as usize) * (part_entry_size as usize);
        if eo + 48 > part_data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&part_data[eo..eo + 36])
            .trim_end_matches('\0')
            .to_string();
        let attributes = u32::from_le_bytes(part_data[eo + 36..eo + 40].try_into().unwrap());
        let first_extent_index =
            u32::from_le_bytes(part_data[eo + 40..eo + 44].try_into().unwrap());
        let num_extents = u32::from_le_bytes(part_data[eo + 44..eo + 48].try_into().unwrap());
        partitions.push(LpPartition {
            name,
            attributes,
            first_extent_index,
            num_extents,
        });
    }

    // 读取 extent 表
    let ext_bytes = (ext_count as u64) * (ext_entry_size as u64);
    let ext_data = read_fn(tables_base + ext_off as u64, ext_bytes)?;
    let mut extents = Vec::with_capacity(ext_count as usize);
    for i in 0..ext_count {
        let eo = (i as usize) * (ext_entry_size as usize);
        if eo + 20 > ext_data.len() {
            break;
        }
        // 24 字节: num_sectors(u64) + target_type(u32) + target_data(u64) + target_source(u32)
        let num_sectors = u64::from_le_bytes(ext_data[eo..eo + 8].try_into().unwrap());
        let _target_type = u32::from_le_bytes(ext_data[eo + 8..eo + 12].try_into().unwrap());
        let target_data = u64::from_le_bytes(ext_data[eo + 12..eo + 20].try_into().unwrap());
        // target_source @ 20..24
        let phys_offset = target_data * 512;
        let size = num_sectors * 512;
        extents.push(LpExtent { phys_offset, size });
    }

    Ok((partitions, extents))
}
