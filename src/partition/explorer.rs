//! super.img 文件系统浏览器（fs_shell）
//!
//! 惰性分层读取，不将整个 super 分区读入内存：
//!   LP metadata(32KB) → ext4 superblock(4KB) → BGD(64B) → inode(256B) → 目录/文件数据块
//!
//! 三层架构：
//!   用户命令 → Explorer (命令解析)
//!            → ext4 读取函数 (只关心分区逻辑偏移)
//!            → LpTranslator (逻辑偏移 → super.img 物理偏移)
//!            → FnMut(u64,u64) 回调读取

use colored::Colorize;
use std::fs;

// ============================================================================
// LP 数据结构
// ============================================================================

/// LP extent 映射: super.img 中的一段连续物理区域
#[derive(Debug, Clone)]
struct LpExtent {
    phys_offset: u64,
    size: u64,
}

/// LP 逻辑分区
#[derive(Debug, Clone)]
struct LpPartition {
    name: String,
    attributes: u32,
    first_extent_index: u32,
    num_extents: u32,
}

/// LP 翻译器: 将分区逻辑偏移翻译为 super.img 物理偏移
#[derive(Debug, Clone)]
struct LpTranslator {
    extents: Vec<LpExtent>,
    total_size: u64,
}

impl LpTranslator {
    /// 创建 identity translator（逻辑偏移 = 物理偏移）
    /// 用于非 super 内的独立分区（如 system_a 直接读取场景）
    fn identity(partition_size: u64) -> Self {
        LpTranslator {
            extents: vec![LpExtent {
                phys_offset: 0,
                size: partition_size,
            }],
            total_size: partition_size,
        }
    }

    /// 简单翻译，不跨 extent，用于 superblock/BGD/inode 等小块
    fn logical_to_abs(&self, logical_offset: u64) -> u64 {
        let mut acc = 0u64;
        for ext in &self.extents {
            if logical_offset < acc + ext.size {
                return ext.phys_offset + (logical_offset - acc);
            }
            acc += ext.size;
        }
        panic!(
            "offset 0x{:X} 超出 LP 范围 (total 0x{:X})",
            logical_offset, self.total_size
        );
    }

    /// 翻译一段连续的分区逻辑偏移（可能跨 LP extent 边界）
    /// 返回多个 (super_img_offset, length) 片段
    fn translate_range(&self, mut pos: u64, mut remaining: u64) -> Vec<(u64, u64)> {
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

// ============================================================================
// LP metadata 解析
// ============================================================================

/// LP magic 值
const LP_GEOMETRY_MAGIC: u32 = 0x674C4164; // AOSP 标准 "gDla"
const LP_GEOMETRY_MAGIC_ALT: u32 = 0x616C4467; // 本设备 "gDla" 变体
const LP_HEADER_MAGIC: u32 = 0x414C5030; // "0PLA"

/// LP 属性位
const LP_ATTR_READONLY: u32 = 0x00000001;
const LP_ATTR_SLOT_SUFFIXED: u32 = 0x00000002;

/// 解析 LP 元数据，只读两块 4KB: geometry @ 0x1000, header @ 0x3000
fn read_lp_metadata<F>(read_fn: &mut F) -> Result<(Vec<LpPartition>, Vec<LpExtent>), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // 1. 读取 geometry @ 0x1000
    let geo_data = read_fn(0x1000, 4096)?;
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
        if eo + part_entry_size as usize > part_data.len() {
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
        if eo + ext_entry_size as usize > ext_data.len() {
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

// ============================================================================
// ext4 常量和结构
// ============================================================================

const EXT4_MAGIC: u16 = 0xEF53;
const EXTENT_HEADER_MAGIC: u16 = 0xF30A;
const ROOT_INODE: u32 = 2;
const EXT4_INODE_FLAG_EXTENTS: u32 = 0x0008_0000;

#[derive(Debug, Clone)]
struct Ext4Superblock {
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u16,
    bgd_entry_size: u16, // 32 或 64，取决于 INCOMPAT_64BIT
}

#[derive(Debug, Clone)]
struct DirEntry {
    inode: u32,
    file_type: u8, // 1=file, 2=dir, 7=symlink
    name: String,
}

// ============================================================================
// ext4 读取函数
// ============================================================================

fn read_ext4_superblock<F>(read_fn: &mut F, lp: &LpTranslator) -> Result<Ext4Superblock, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let abs_off = lp.logical_to_abs(1024);
    let data = read_fn(abs_off, 1024)?;
    if data.len() < 1024 {
        return Err("superblock 数据不足 1024 字节".into());
    }
    let magic = u16::from_le_bytes(data[56..58].try_into().unwrap());
    if magic != EXT4_MAGIC {
        return Err(format!(
            "ext4 magic 不匹配: 0x{:04X} (期望 0x{:04X})",
            magic, EXT4_MAGIC
        ));
    }
    let block_size = 1024u32 << u32::from_le_bytes(data[24..28].try_into().unwrap());
    let inodes_per_group = u32::from_le_bytes(data[40..44].try_into().unwrap());
    let inode_size = u16::from_le_bytes(data[88..90].try_into().unwrap());
    let feature_incompat = u32::from_le_bytes(data[96..100].try_into().unwrap());
    // INCOMPAT_64BIT (bit 5 = 0x20) 表示 64 字节 BGD 条目
    let bgd_entry_size: u16 = if (feature_incompat & 0x20) != 0 {
        64
    } else {
        32
    };
    Ok(Ext4Superblock {
        block_size,
        inodes_per_group,
        inode_size: if inode_size == 0 { 128 } else { inode_size },
        bgd_entry_size,
    })
}

/// 读取指定 inode 的原始数据
fn read_inode<F>(
    read_fn: &mut F,
    lp: &LpTranslator,
    sb: &Ext4Superblock,
    inode_num: u32,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let group = (inode_num - 1) / sb.inodes_per_group;

    // BGD 表位于 block 1 (block_size > 1024 时) 或 block 2
    let bgd_base = if sb.block_size > 1024 {
        sb.block_size as u64
    } else {
        2048
    };
    let bgd_entry_offset = bgd_base + (group as u64) * sb.bgd_entry_size as u64;
    let bgd_abs = lp.logical_to_abs(bgd_entry_offset);
    let bgd_data = read_fn(bgd_abs, sb.bgd_entry_size as u64)?;
    let inode_table_block = u32::from_le_bytes(bgd_data[8..12].try_into().unwrap());

    // 计算 inode 在分区内的逻辑偏移
    let inode_logical_offset = inode_table_block as u64 * sb.block_size as u64
        + (inode_num - 1) as u64 * sb.inode_size as u64;

    // 通过 LP 翻译
    let abs_offset = lp.logical_to_abs(inode_logical_offset);
    let inode_data = read_fn(abs_offset, sb.inode_size as u64)?;
    Ok(inode_data)
}

/// 获取 inode 文件大小 (i_size_lo | i_size_hi << 32)
fn get_inode_size(inode_data: &[u8]) -> u64 {
    let size_lo = u32::from_le_bytes(inode_data[4..8].try_into().unwrap());
    let size_hi = if inode_data.len() >= 112 {
        u32::from_le_bytes(inode_data[108..112].try_into().unwrap())
    } else {
        0
    };
    (size_lo as u64) | ((size_hi as u64) << 32)
}

/// 获取 inode mode (u16 @ offset 0)
fn get_inode_mode(inode_data: &[u8]) -> u16 {
    u16::from_le_bytes(inode_data[0..2].try_into().unwrap())
}

/// 获取 inode flags (u32 @ offset 32)
fn get_inode_flags(inode_data: &[u8]) -> u32 {
    u32::from_le_bytes(inode_data[32..36].try_into().unwrap())
}

/// 判断 inode 是否为 symlink（mode & 0xF000 == 0xA000）
fn is_symlink_mode(mode: u16) -> bool {
    (mode & 0xF000) == 0xA000
}

/// 递归解析 extent 树，返回所有叶子节点的 (物理起始, 物理结束) 范围列表
///
/// 支持 depth=0（叶子节点）和 depth>=1（索引节点）。
/// 对于索引节点，递归读取子块并解析其 extent entries。
fn parse_extent_tree<F>(
    read_fn: &mut F,
    lp: &LpTranslator,
    sb: &Ext4Superblock,
    i_block_data: &[u8],
) -> Result<Vec<(u64, u64)>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    if i_block_data.len() < 12 {
        return Ok(vec![]);
    }
    let magic = u16::from_le_bytes(i_block_data[0..2].try_into().unwrap());
    if magic != EXTENT_HEADER_MAGIC {
        return Ok(vec![]);
    }
    let entries = u16::from_le_bytes(i_block_data[2..4].try_into().unwrap());
    let depth = u16::from_le_bytes(i_block_data[6..8].try_into().unwrap());
    let block_size = sb.block_size as u64;

    let mut ranges = Vec::new();

    for i in 0..entries {
        let off = 12 + (i as usize) * 12;
        if off + 12 > i_block_data.len() {
            break;
        }
        let _lblock = u32::from_le_bytes(i_block_data[off..off + 4].try_into().unwrap());
        let len = u16::from_le_bytes(i_block_data[off + 4..off + 6].try_into().unwrap());
        let start_hi = u16::from_le_bytes(i_block_data[off + 6..off + 8].try_into().unwrap());
        let start_lo = u32::from_le_bytes(i_block_data[off + 8..off + 12].try_into().unwrap());
        let pblock = ((start_hi as u64) << 32) | (start_lo as u64);

        if depth == 0 {
            // 叶子节点：直接收集物理范围
            let start = pblock * block_size;
            let end = (pblock + len as u64) * block_size;
            ranges.push((start, end));
        } else {
            // 索引节点：读取子块并递归解析
            let child_offset = pblock * block_size;
            let fragments = lp.translate_range(child_offset, block_size);
            let mut child_data = Vec::new();
            for (abs_off, frag_len) in fragments {
                let chunk = read_fn(abs_off, frag_len)?;
                child_data.extend_from_slice(&chunk);
            }
            let child_ranges = parse_extent_tree(read_fn, lp, sb, &child_data)?;
            ranges.extend_from_slice(&child_ranges);
        }
    }
    Ok(ranges)
}

/// 读取 inode 的所有数据块（通过 extent + LP 翻译）
/// 支持三种情况：
///   1. EXT4_INODE_FLAG_EXTENTS → 标准 extent 树读取（支持 depth=0/1）
///   2. symlink（无 EXTENTS）→ 内联数据（i_block 区域，最大 60 字节）
///   3. 空目录/空文件（无 EXTENTS，size=0）→ 返回空
fn read_inode_data<F>(
    read_fn: &mut F,
    lp: &LpTranslator,
    sb: &Ext4Superblock,
    inode_data: &[u8],
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let flags = get_inode_flags(inode_data);
    let mode = get_inode_mode(inode_data);
    let file_size = get_inode_size(inode_data);

    // 空文件/空目录
    if file_size == 0 {
        return Ok(Vec::new());
    }

    // symlink 内联数据：目标路径直接存储在 i_block 区域（最大 60 字节）
    if is_symlink_mode(mode) && (flags & EXT4_INODE_FLAG_EXTENTS) == 0 {
        let i_block = &inode_data[40..100.min(inode_data.len())];
        let symlink_len = file_size.min(60) as usize;
        return Ok(i_block[..symlink_len].to_vec());
    }

    // 非 symlink 且无 extents：旧式 block map（间接块），暂不支持
    if (flags & EXT4_INODE_FLAG_EXTENTS) == 0 {
        return Err("inode 不使用 extents（不支持旧版 ext 格式）".into());
    }

    // i_block @ offset 40, 60 bytes
    let i_block = &inode_data[40..100.min(inode_data.len())];
    let magic = u16::from_le_bytes(i_block[0..2].try_into().unwrap());
    if magic != EXTENT_HEADER_MAGIC {
        // vendor/overlay 等 COW (copy-on-write) 分区的 inode 可能含有非标准 magic
        // 返回空数据而非报错，让 ls/cat 优雅处理
        return Ok(Vec::new());
    }

    let ranges = parse_extent_tree(read_fn, lp, sb, i_block)?;

    let mut data = Vec::with_capacity(file_size as usize);
    for (start, end) in ranges {
        let read_len = end - start;
        let fragments = lp.translate_range(start, read_len);
        for (abs_off, frag_len) in fragments {
            let chunk = read_fn(abs_off, frag_len)?;
            data.extend_from_slice(&chunk);
        }
    }

    if data.len() > file_size as usize {
        data.truncate(file_size as usize);
    }
    Ok(data)
}

/// 解析目录条目
fn parse_dir(data: &[u8]) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let inode = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        let rec_len = u16::from_le_bytes(data[offset + 4..offset + 6].try_into().unwrap());
        if rec_len == 0 || rec_len as usize > data.len() - offset {
            break;
        }
        if inode != 0 {
            let name_len = data[offset + 6] as usize;
            let file_type = data[offset + 7];
            let name = String::from_utf8_lossy(&data[offset + 8..offset + 8 + name_len])
                .trim_end_matches('\0')
                .to_string();
            entries.push(DirEntry {
                inode,
                file_type,
                name,
            });
        }
        offset += rec_len as usize;
    }
    entries
}

/// 按路径逐级解析到目标 inode
fn resolve_path<F>(
    read_fn: &mut F,
    lp: &LpTranslator,
    sb: &Ext4Superblock,
    start_inode: u32,
    path: &str,
) -> Result<u32, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let mut current_inode = start_inode;
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let inode_data = read_inode(read_fn, lp, sb, current_inode)?;
        let dir_data = read_inode_data(read_fn, lp, sb, &inode_data)?;
        let entries = parse_dir(&dir_data);
        let found = entries
            .iter()
            .find(|e| e.name == segment)
            .ok_or_else(|| format!("未找到: '{}'", segment))?;
        current_inode = found.inode;
    }
    Ok(current_inode)
}

// ============================================================================
// Explorer 结构体
// ============================================================================

pub struct Explorer {
    partitions: Vec<LpPartition>,
    all_extents: Vec<LpExtent>,
    current_partition: Option<String>,
    current_path: String,
}

impl Explorer {
    /// 按名称查找分区，自动补 _b 后缀
    fn find_partition(&self, name: &str) -> Result<usize, String> {
        // 精确匹配
        for (i, p) in self.partitions.iter().enumerate() {
            if p.name == name {
                return Ok(i);
            }
        }
        // 尝试 _b / _a 后缀
        for suffix in &["_b", "_a"] {
            let candidate = format!("{}{}", name, suffix);
            for (i, p) in self.partitions.iter().enumerate() {
                if p.name == candidate {
                    return Ok(i);
                }
            }
        }
        Err(format!("未找到分区: {}", name))
    }

    /// 构造指定分区的 LpTranslator
    fn make_translator(&self, part_idx: usize) -> Result<LpTranslator, String> {
        let part = &self.partitions[part_idx];
        let mut extents = Vec::new();
        let mut total_size = 0u64;
        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                extents.push(ext.clone());
                total_size += ext.size;
            }
        }
        Ok(LpTranslator {
            extents,
            total_size,
        })
    }

    /// 计算分区总大小
    fn partition_size(&self, part_idx: usize) -> u64 {
        let part = &self.partitions[part_idx];
        let mut total = 0u64;
        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                total += ext.size;
            }
        }
        total
    }

    /// 拆分 target: "system_b/system/etc" → ("system_b", "system/etc")
    fn split_target(&self, target: &str) -> Result<(String, String), String> {
        let target = target.trim_start_matches('/');
        if target.is_empty() {
            return Ok(("".to_string(), "".to_string()));
        }
        // 尝试匹配分区名（第一段或前两段）
        // 先尝试完整第一段作为分区名
        let first_slash = target.find('/');
        let first_seg = if let Some(pos) = first_slash {
            &target[..pos]
        } else {
            return Ok((target.to_string(), "".to_string()));
        };

        // 精确匹配
        if self.find_partition(first_seg).is_ok() {
            Ok((
                first_seg.to_string(),
                target[first_slash.unwrap() + 1..].to_string(),
            ))
        } else {
            Err(format!("未找到分区: {}", first_seg))
        }
    }
}

// ============================================================================
// 工具函数
// ============================================================================

fn fmt_size(n: u64) -> String {
    if n < 1024 {
        format!("{}B", n)
    } else if n < 1048576 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else if n < 1073741824 {
        format!("{:.1}MB", n as f64 / 1048576.0)
    } else {
        format!("{:.2}GB", n as f64 / 1073741824.0)
    }
}

fn fmt_inode_size(n: u64) -> String {
    if n < 1024 {
        format!("{}", n)
    } else if n < 1048576 {
        format!("{:.0}K", n as f64 / 1024.0)
    } else if n < 1073741824 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else {
        format!("{:.1}G", n as f64 / 1073741824.0)
    }
}

fn is_dir_mode(mode: u16) -> bool {
    (mode & 0xF000) == 0x4000
}

fn is_file_mode(mode: u16) -> bool {
    (mode & 0xF000) == 0x8000
}

fn file_type_str(ft: u8) -> &'static str {
    match ft {
        1 => "FILE",
        2 => "DIR ",
        7 => "LINK",
        _ => "????",
    }
}

// ============================================================================
// 命令实现
// ============================================================================

impl Explorer {
    /// ls super — 列出所有 LP 分区
    fn cmd_ls_super(&self) {
        println!("{:<20} {:>12} {:>12}  {}", "分区", "偏移", "大小", "标记");
        println!("{}", "-".repeat(60));
        for part in &self.partitions {
            let (mut total_size, mut first_offset) = (0u64, 0u64);
            let mut first = true;
            for i in 0..part.num_extents {
                if let Some(ext) = self.all_extents.get((part.first_extent_index + i) as usize) {
                    if first {
                        first_offset = ext.phys_offset;
                        first = false;
                    }
                    total_size += ext.size;
                }
            }
            let mut tags = Vec::new();
            if (part.attributes & LP_ATTR_READONLY) != 0 {
                tags.push("RO");
            }
            if (part.attributes & LP_ATTR_SLOT_SUFFIXED) != 0 {
                if part.name.ends_with("_a") {
                    tags.push("[A-slot]");
                } else if part.name.ends_with("_b") {
                    tags.push("[B-slot]");
                }
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                tags.join(" ")
            };
            println!(
                "{:<20} 0x{:>08X} {:>12}  {}",
                part.name,
                first_offset,
                fmt_size(total_size),
                tag_str,
            );
        }
    }

    /// ls [partition][/path] — 列出目录内容
    fn cmd_ls<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let trimmed = target.trim();
        if trimmed.is_empty() || trimmed == "super" {
            if self.current_partition.is_none() {
                self.cmd_ls_super();
                return Ok(());
            }
            // 有当前分区时，列出当前路径
            let part_name = self.current_partition.as_ref().unwrap().clone();
            let sub_path = self.current_path.clone();
            // 构造完整 target: part_name/sub_path
            let full_target = if sub_path.is_empty() {
                part_name
            } else {
                format!("{}/{}", part_name, sub_path)
            };
            return self.cmd_ls_path(read_fn, &full_target, "");
        }

        // 处理相对路径：不以 / 开头且当前在分区内
        if !trimmed.starts_with('/') {
            if let Some(ref cur_part) = self.current_partition {
                // 先检查是否是分区名
                if self.find_partition(trimmed).is_err() {
                    // 不是分区名，拼接当前路径
                    let full_target = if self.current_path.is_empty() {
                        format!("{}", cur_part)
                    } else {
                        format!("{}/{}", cur_part, self.current_path)
                    };
                    // 拼接 target
                    let full_target = if trimmed.is_empty() {
                        full_target
                    } else {
                        format!("{}/{}", full_target, trimmed)
                    };
                    return self.cmd_ls_path(read_fn, &full_target, "");
                }
            }
        }

        self.cmd_ls_path(read_fn, target, "")
    }

    fn cmd_ls_path<F>(&self, read_fn: &mut F, target: &str, _unused: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() {
            self.cmd_ls_super();
            return Ok(());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let dir_inode = if sub_path.is_empty() {
            ROOT_INODE
        } else {
            resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?
        };

        let inode_data = read_inode(read_fn, &lp, &sb, dir_inode)?;
        let mode = get_inode_mode(&inode_data);
        let size = get_inode_size(&inode_data);

        if is_file_mode(mode) {
            // 单文件：显示文件信息
            let mut ft = "FILE".to_string();
            if (get_inode_flags(&inode_data) & EXT4_INODE_FLAG_EXTENTS) != 0 {
                ft = "FILE(ext)".to_string();
            }
            println!("{:<40} {:>10}  {}", part_name, fmt_inode_size(size), ft);
            return Ok(());
        }

        // 目录
        let dir_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
        let entries = parse_dir(&dir_data);

        // 分离目录和文件
        let mut dirs: Vec<&DirEntry> = Vec::new();
        let mut files: Vec<&DirEntry> = Vec::new();
        for e in &entries {
            if e.name == "." || e.name == ".." {
                continue;
            }
            match e.file_type {
                2 => dirs.push(e),
                _ => files.push(e),
            }
        }
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));

        // 获取每个条目的大小（需要读 inode）
        for d in &dirs {
            match read_inode(read_fn, &lp, &sb, d.inode) {
                Ok(ino_data) => {
                    let ino_size = get_inode_size(&ino_data);
                    // inode 数据损坏时大小可能离奇，限制显示
                    let display_size = if ino_size > 0x1_0000_0000 {
                        4096u64
                    } else {
                        ino_size
                    };
                    println!(
                        "{:<40} {:>10}  {}",
                        format!("{}/", d.name),
                        fmt_inode_size(display_size),
                        file_type_str(d.file_type).dimmed(),
                    );
                }
                Err(_) => {
                    println!(
                        "{:<40} {:>10}  {}",
                        format!("{}/", d.name),
                        "4K",
                        file_type_str(d.file_type).dimmed(),
                    );
                }
            }
        }
        for f in &files {
            match read_inode(read_fn, &lp, &sb, f.inode) {
                Ok(ino_data) => {
                    let ino_size = get_inode_size(&ino_data);
                    // inode 数据损坏时大小可能离奇，限制显示
                    let display_size = if ino_size > 0x1_0000_0000 {
                        4096u64
                    } else {
                        ino_size
                    };
                    println!(
                        "{:<40} {:>10}  {}",
                        f.name,
                        fmt_inode_size(display_size),
                        file_type_str(f.file_type).dimmed(),
                    );
                }
                Err(_) => {
                    println!(
                        "{:<40} {:>10}  {}",
                        f.name,
                        "???",
                        file_type_str(f.file_type).dimmed(),
                    );
                }
            }
        }

        println!(
            "\n共 {} 项 ({} 目录, {} 文件)",
            dirs.len() + files.len(),
            dirs.len(),
            files.len()
        );
        Ok(())
    }

    /// cat <partition/path> — 显示文件内容
    fn cmd_cat<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() || sub_path.is_empty() {
            return Err("用法: cat <partition>/<path>".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
        let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
        let mode = get_inode_mode(&inode_data);
        let size = get_inode_size(&inode_data);

        if is_dir_mode(mode) {
            // 目录 → 列出条目
            let dir_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
            let entries = parse_dir(&dir_data);
            println!("(目录) {} 条目:", entries.len());
            for e in &entries {
                if e.inode != 0 {
                    let ino_data = read_inode(read_fn, &lp, &sb, e.inode)?;
                    let ino_size = get_inode_size(&ino_data);
                    println!(
                        "  {}  {:>10}  {}",
                        file_type_str(e.file_type),
                        fmt_inode_size(ino_size),
                        e.name,
                    );
                }
            }
            return Ok(());
        }

        // 文件
        let file_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
        println!("[{}, {} bytes]", part_name, size);

        match String::from_utf8(file_data.clone()) {
            Ok(text) => {
                for line in text.lines() {
                    println!(" {}", line);
                }
            }
            Err(_) => {
                println!("[binary, {} bytes]", file_data.len());
                // hex dump 前 512 字节
                let dump_len = 512.min(file_data.len());
                for row in 0..dump_len / 16 {
                    let off = row * 16;
                    let hex: Vec<String> = file_data[off..off + 16]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect();
                    let ascii: String = file_data[off..off + 16]
                        .iter()
                        .map(|&b| {
                            if b >= 0x20 && b < 0x7F {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    println!("  {:08X}: {}  {}", off, hex.join(" "), ascii);
                }
            }
        }
        Ok(())
    }

    /// cd <partition>[/path] — 切换当前分区/目录
    fn cmd_cd<F>(&mut self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let trimmed = target.trim();
        if trimmed.is_empty() || trimmed == "/" || trimmed == "super" {
            self.current_partition = None;
            self.current_path.clear();
            return Ok(());
        }

        // 处理 ".."：从当前路径回退一级
        if trimmed == ".." {
            if let Some(ref _part) = self.current_partition {
                if self.current_path.is_empty() {
                    // 已在分区根目录，回到 super 根
                    self.current_partition = None;
                } else if let Some(last_slash) = self.current_path.rfind('/') {
                    self.current_path = self.current_path[..last_slash].to_string();
                } else {
                    self.current_path.clear();
                }
            }
            return Ok(());
        }

        // 绝对路径：以 / 开头
        if trimmed.starts_with('/') {
            let target = trimmed.trim_start_matches('/');
            let (part_name, sub_path) = self.split_target(target)?;
            if part_name.is_empty() {
                self.current_partition = None;
                self.current_path.clear();
                return Ok(());
            }
            // 获取实际分区名
            let part_idx = self.find_partition(&part_name)?;
            let real_name = self.partitions[part_idx].name.clone();

            // 验证子路径是目录
            if !sub_path.is_empty() {
                let lp = self.make_translator(part_idx)?;
                let sb = read_ext4_superblock(read_fn, &lp)?;
                let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
                let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
                let mode = get_inode_mode(&inode_data);
                if !is_dir_mode(mode) {
                    return Err("不是目录".into());
                }
            }

            self.current_partition = Some(real_name);
            self.current_path = sub_path;
            return Ok(());
        }

        // 相对路径：当前已在某分区内时，优先尝试子目录
        if let Some(ref cur_part) = self.current_partition {
            let part_idx = self.find_partition(cur_part)?;
            let real_name = self.partitions[part_idx].name.clone();
            let lp = self.make_translator(part_idx)?;
            let sb = read_ext4_superblock(read_fn, &lp)?;

            // 先尝试将 target 当作当前目录下的子路径
            let try_path = if self.current_path.is_empty() {
                trimmed.to_string()
            } else {
                format!("{}/{}", self.current_path, trimmed)
            };

            // 尝试解析子路径
            match resolve_path(read_fn, &lp, &sb, ROOT_INODE, &try_path) {
                Ok(inode_num) => {
                    let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
                    let mode = get_inode_mode(&inode_data);
                    if is_dir_mode(mode) {
                        self.current_partition = Some(real_name);
                        self.current_path = try_path;
                        return Ok(());
                    } else {
                        return Err("不是目录".into());
                    }
                }
                Err(_) => {
                    // 子路径不存在，尝试切换到其他分区
                }
            }
        }

        // 切换分区（可能带子路径）
        let (part_name, sub_path) = self.split_target(trimmed)?;
        if part_name.is_empty() {
            self.current_partition = None;
            self.current_path.clear();
            return Ok(());
        }

        let part_idx = self.find_partition(&part_name)?;
        let real_name = self.partitions[part_idx].name.clone();

        // 验证子路径是目录
        if !sub_path.is_empty() {
            let lp = self.make_translator(part_idx)?;
            let sb = read_ext4_superblock(read_fn, &lp)?;
            let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
            let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
            let mode = get_inode_mode(&inode_data);
            if !is_dir_mode(mode) {
                return Err("不是目录".into());
            }
        }

        self.current_partition = Some(real_name);
        self.current_path = sub_path;
        Ok(())
    }

    /// pwd — 显示当前路径
    fn cmd_pwd(&self) {
        match &self.current_partition {
            None => println!("/"),
            Some(part) => {
                if self.current_path.is_empty() {
                    println!("/{}/", part);
                } else {
                    println!("/{}/{}", part, self.current_path);
                }
            }
        }
    }

    /// tree <part>[/path] [depth] — 树形显示
    fn cmd_tree<F>(&self, read_fn: &mut F, arg: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let parts: Vec<&str> = arg.splitn(2, ' ').collect();
        let target = parts[0].trim();
        let max_depth: usize = if parts.len() > 1 {
            parts[1].trim().parse().unwrap_or(3)
        } else {
            3
        };

        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() {
            return Err("用法: tree <partition>[/path] [depth]".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let start_inode = if sub_path.is_empty() {
            ROOT_INODE
        } else {
            resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?
        };

        println!("{}", part_name);
        self.tree_recurse(read_fn, &lp, &sb, start_inode, "", max_depth, max_depth)
    }

    fn tree_recurse<F>(
        &self,
        read_fn: &mut F,
        lp: &LpTranslator,
        sb: &Ext4Superblock,
        inode_num: u32,
        prefix: &str,
        depth: usize,
        max_depth: usize,
    ) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        if depth == 0 {
            return Ok(());
        }

        let inode_data = read_inode(read_fn, lp, sb, inode_num)?;
        let dir_data = match read_inode_data(read_fn, lp, sb, &inode_data) {
            Ok(d) => d,
            Err(e) => {
                // 非 extent inode / 旧式 ext 格式等：跳过不崩溃
                println!("{}(读取失败: {})", prefix, e);
                return Ok(());
            }
        };
        let entries = parse_dir(&dir_data);

        // 分离目录和文件，排序
        let mut items: Vec<&DirEntry> = entries
            .iter()
            .filter(|e| e.inode != 0 && e.name != "." && e.name != "..")
            .collect();
        items.sort_by(|a, b| {
            // 目录优先
            let a_is_dir = a.file_type == 2;
            let b_is_dir = b.file_type == 2;
            if a_is_dir != b_is_dir {
                return b_is_dir.cmp(&a_is_dir);
            }
            a.name.cmp(&b.name)
        });

        let last_idx = items.len().saturating_sub(1);
        for (i, entry) in items.iter().enumerate() {
            let is_last = i == last_idx;
            let connector = if is_last { "+-- " } else { "|-- " };
            let name_suffix = if entry.file_type == 2 { "/" } else { "" };
            println!("{}{}{}{}", prefix, connector, entry.name, name_suffix);

            if entry.file_type == 2 {
                let new_prefix = if is_last {
                    format!("{}    ", prefix)
                } else {
                    format!("{}|   ", prefix)
                };
                self.tree_recurse(
                    read_fn,
                    lp,
                    sb,
                    entry.inode,
                    &new_prefix,
                    depth - 1,
                    max_depth,
                )?;
            }
        }
        Ok(())
    }

    /// info <partition> — 分区详情
    fn cmd_info<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let part_idx = self.find_partition(target)?;
        let part = &self.partitions[part_idx];
        let total_size = self.partition_size(part_idx);

        println!("分区名: {}", part.name);
        println!("大小:   {} ({} bytes)", fmt_size(total_size), total_size);

        let mut tags = Vec::new();
        if (part.attributes & LP_ATTR_READONLY) != 0 {
            tags.push("READONLY");
        }
        if (part.attributes & LP_ATTR_SLOT_SUFFIXED) != 0 {
            if part.name.ends_with("_a") {
                tags.push("[A-slot]");
            } else if part.name.ends_with("_b") {
                tags.push("[B-slot]");
            }
        }
        println!(
            "标记:   {}",
            if tags.is_empty() {
                "无".to_string()
            } else {
                tags.join(" ")
            }
        );
        println!("Extent:  {} 段", part.num_extents);

        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                println!(
                    "  [{}/{}] offset=0x{:08X} size={}",
                    i + 1,
                    part.num_extents,
                    ext.phys_offset,
                    fmt_size(ext.size),
                );
            }
        }

        // 尝试读 ext4 superblock
        let lp = self.make_translator(part_idx)?;
        match read_ext4_superblock(read_fn, &lp) {
            Ok(sb) => {
                println!();
                println!("文件系统: ext4");
                println!("块大小:   {} bytes", sb.block_size);
                println!("Inode/组: {}", sb.inodes_per_group);
                println!("Inode大小: {} bytes", sb.inode_size);
            }
            Err(_) => {
                println!();
                println!("文件系统: 未知（无法读取 ext4 superblock）");
            }
        }

        Ok(())
    }

    /// cp [-r] [--depth N] <partition/path> <local_path> — 提取到本地
    ///   -r          递归复制目录（默认只复制文件，目录会报错）
    ///   --depth N   限制递归深度（1=只复制当前目录文件，2=包含子目录，等）
    fn cmd_cp<F>(&self, read_fn: &mut F, arg: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        // 解析参数
        let tokens: Vec<&str> = arg.split_whitespace().collect();
        let mut recursive = false;
        let mut max_depth: Option<usize> = None;
        let mut src_idx = 0usize;
        let mut dst_idx = tokens.len().saturating_sub(1);

        let mut i = 0;
        while i < tokens.len() {
            match tokens[i] {
                "-r" | "-R" | "--recursive" => {
                    recursive = true;
                    i += 1;
                }
                "-d" | "--depth" => {
                    if i + 1 < tokens.len() {
                        max_depth = tokens[i + 1].parse().ok();
                        i += 2;
                    } else {
                        return Err("--depth 后缺少数值".into());
                    }
                }
                _ => {
                    if src_idx == 0 && !tokens[i].starts_with('-') {
                        src_idx = i;
                    }
                    dst_idx = i;
                    i += 1;
                }
            }
        }

        if src_idx >= tokens.len() || dst_idx >= tokens.len() || src_idx == dst_idx {
            return Err("用法: cp [-r] [--depth N] <partition/path> <local_path>".into());
        }

        let src_raw = tokens[src_idx];
        let dst_raw = tokens[dst_idx];

        let (part_name, sub_path) = self.split_target(src_raw)?;
        if part_name.is_empty() || sub_path.is_empty() {
            return Err("用法: cp [-r] [--depth N] <partition/path> <local_path>".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
        let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
        let mode = get_inode_mode(&inode_data);

        // 构造目标路径
        let dst = if dst_raw.ends_with('\\') || dst_raw.ends_with('/') {
            // 目标是目录
            let file_name = sub_path.rsplit('/').next().unwrap_or("unknown");
            let sep = if dst_raw.ends_with('\\') { "\\" } else { "/" };
            format!("{}{}{}", dst_raw, sep, file_name)
        } else {
            dst_raw.to_string()
        };

        if is_dir_mode(mode) {
            if !recursive {
                return Err(format!("{} 是目录，请使用 -r 选项递归复制", src_raw));
            }
            // 递归复制目录，应用 max_depth
            let effective_max = max_depth.unwrap_or(usize::MAX);
            println!(
                "递归复制: {} → {} (max_depth={})",
                src_raw, dst, effective_max
            );
            self.cp_recurse_depth(
                read_fn,
                &lp,
                &sb,
                inode_num,
                &dst,
                &sub_path,
                0,
                effective_max,
            )?;
        } else {
            // 复制文件
            println!("提取: {} → {}", src_raw, dst);
            let file_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
            fs::write(&dst, &file_data).map_err(|e| format!("写入失败: {}", e))?;
            println!("完成: {} ({} bytes)", dst, file_data.len());
        }

        Ok(())
    }

    fn cp_recurse_depth<F>(
        &self,
        read_fn: &mut F,
        lp: &LpTranslator,
        sb: &Ext4Superblock,
        inode_num: u32,
        dst_dir: &str,
        src_prefix: &str,
        depth: usize,
        max_depth: usize,
    ) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        // 达到用户指定的最大深度时停止递归，创建空目录作为占位
        if depth >= max_depth {
            if max_depth < usize::MAX {
                println!(
                    "  [depth={}/{}] 达到最大深度，跳过子目录: {}",
                    depth, max_depth, src_prefix
                );
            }
            let _ = fs::create_dir_all(dst_dir);
            return Ok(());
        }

        // 硬编码安全上限：ext4 最大深度约 4000+
        const MAX_DEPTH_HARD: usize = 256;
        if depth > MAX_DEPTH_HARD {
            eprintln!(
                "  [跳过] 目录过深 (>{}) 跳过递归: {}",
                MAX_DEPTH_HARD, src_prefix
            );
            let _ = fs::create_dir_all(dst_dir);
            return Ok(());
        }

        if let Err(e) = fs::create_dir_all(dst_dir) {
            eprintln!("  [警告] 创建目录失败 {}: {}", dst_dir, e);
            return Ok(());
        }

        let inode_data = match read_inode(read_fn, lp, sb, inode_num) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [跳过] 读取 inode {} 失败: {}", inode_num, e);
                return Ok(());
            }
        };

        let dir_data = match read_inode_data(read_fn, lp, sb, &inode_data) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [跳过] 读取目录数据失败 {}: {}", src_prefix, e);
                return Ok(());
            }
        };

        let entries = parse_dir(&dir_data);

        for entry in &entries {
            if entry.inode == 0 || entry.name == "." || entry.name == ".." {
                continue;
            }

            let entry_path = format!("{}/{}", src_prefix, entry.name);
            let dst_path = format!("{}/{}", dst_dir, entry.name);

            let entry_inode_data = match read_inode(read_fn, lp, sb, entry.inode) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "  [跳过] 无法读取 {} (inode={}): {}",
                        entry_path, entry.inode, e
                    );
                    // 创建空文件作为占位
                    let _ = fs::write(&dst_path, b"");
                    continue;
                }
            };

            let mode = get_inode_mode(&entry_inode_data);

            if is_dir_mode(mode) {
                if let Err(e) = self.cp_recurse_depth(
                    read_fn,
                    lp,
                    sb,
                    entry.inode,
                    &dst_path,
                    &entry_path,
                    depth + 1,
                    max_depth,
                ) {
                    eprintln!("  [警告] 子目录复制失败 {}: {}", entry_path, e);
                }
            } else {
                match read_inode_data(read_fn, lp, sb, &entry_inode_data) {
                    Ok(file_data) => {
                        if let Err(e) = fs::write(&dst_path, &file_data) {
                            eprintln!("  [跳过] 写入失败 {}: {}", dst_path, e);
                            // 创建空文件作为占位
                            let _ = fs::write(&dst_path, b"");
                        } else {
                            println!("  提取: {} ({} bytes)", entry_path, file_data.len());
                        }
                    }
                    Err(e) => {
                        eprintln!("  [跳过] 读取文件失败 {}: {}", entry_path, e);
                        // 创建空文件作为占位
                        let _ = fs::write(&dst_path, b"");
                    }
                }
            }
        }

        Ok(())
    }

    /// 帮助
    fn print_help() {
        println!("命令:");
        println!("  ls [super]                 列出 super 分区表");
        println!("  ls [part[/path]]           列出目录内容");
        println!("  cat <part/path>            显示文件内容");
        println!("  cp [-r] [--depth N] <part/path> <local>  提取文件/目录到本地");
        println!("      -r        递归复制目录");
        println!("      --depth N 限制递归深度 (1=当前目录, 2=一层子目录...)");
        println!("  cd <part>[/path]           切换当前分区/目录");
        println!("  pwd                        显示当前路径");
        println!("  tree <part>[/path] [depth] 树形显示 (默认深度3)");
        println!("  info <part>                分区详情");
        println!("  help                       帮助");
        println!("  quit / exit                退出");
    }
}

// ============================================================================
// 交互模式
// ============================================================================

impl Explorer {
    pub fn run_interactive<F>(&mut self, read_fn: &mut F)
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        println!("\n{}", "zyb fs_shell - super.img 浏览器".bold());
        println!("输入 'help' 查看命令\n");

        loop {
            // 检查 Ctrl+C 状态
            if crate::cancel::force_requested() {
                println!("\n强制退出 fs_shell。");
                crate::cancel::reset();
                break;
            }
            if crate::cancel::requested() {
                println!("\n(已取消，输入 help 查看命令，再次 Ctrl+C 强制退出)");
                crate::cancel::reset();
                continue;
            }

            // 提示符
            match &self.current_partition {
                None => print!("{}", "zyb> ".cyan()),
                Some(part) => {
                    if self.current_path.is_empty() {
                        print!("{}", format!("zyb:/{}/> ", part).cyan());
                    } else {
                        print!(
                            "{}",
                            format!("zyb:/{}/{}> ", part, self.current_path).cyan()
                        );
                    }
                }
            };
            use std::io::Write;
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            match std::io::stdin().read_line(&mut input) {
                Ok(0) => {
                    // EOF / Ctrl+D
                    println!("\n再见!");
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    println!("读取输入失败");
                    break;
                }
            }

            // read_line 之后也检查 cancel
            if crate::cancel::force_requested() {
                println!("\n强制退出 fs_shell。");
                crate::cancel::reset();
                break;
            }
            if crate::cancel::requested() {
                crate::cancel::reset();
                continue;
            }

            let line = input.trim();
            if line.is_empty() {
                continue;
            }

            // 拆分命令和参数
            let (cmd, arg) = if let Some(pos) = line.find(' ') {
                (&line[..pos], line[pos + 1..].trim())
            } else {
                (line, "")
            };

            match cmd {
                "ls" => {
                    if let Err(e) = self.cmd_ls(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cat" => {
                    if let Err(e) = self.cmd_cat(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cp" => {
                    if let Err(e) = self.cmd_cp(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cd" => {
                    if let Err(e) = self.cmd_cd(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "pwd" => {
                    self.cmd_pwd();
                }
                "tree" => {
                    if let Err(e) = self.cmd_tree(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "info" => {
                    if let Err(e) = self.cmd_info(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "help" | "?" => {
                    Self::print_help();
                }
                "quit" | "exit" | "q" => {
                    println!("再见!");
                    break;
                }
                _ => {
                    println!(
                        "{}",
                        format!("未知命令: {} (输入 help 查看帮助)", cmd).red()
                    );
                }
            }
        }
    }
}

// ============================================================================
// 公开入口
// ============================================================================

/// fs_shell 入口函数
pub fn run_explorer<F>(read_fn: &mut F) -> Result<(), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // banner 在 run_interactive 中打印，此处不再重复

    let (partitions, extents) = read_lp_metadata(read_fn)?;
    println!("解析到 {} 个逻辑分区:", partitions.len());
    for p in &partitions {
        println!("  {}", p.name);
    }
    println!();

    let mut explorer = Explorer {
        partitions,
        all_extents: extents,
        current_partition: None,
        current_path: String::new(),
    };

    explorer.run_interactive(read_fn);
    Ok(())
}


/// 从 ext4 分区中按路径读取文件内容（不依赖 LP 元数据）
///
/// 复用 fs_shell 内部的 ext4 解析逻辑（支持 BGD 32/64 字节、inline symlink、
/// 空 inode 等），使用 identity translator 直接按分区偏移读取。
///
/// 参数：
/// - `read_fn`: 读取回调 `(offset, len) -> Result<Vec<u8>, String>`，offset 从分区起始算
/// - `partition_size`: 分区总大小
/// - `path`: 文件路径，如 `"system/build.prop"`
///
/// 返回：文件原始数据
pub fn read_file_by_path<F>(
    read_fn: &mut F,
    partition_size: u64,
    path: &str,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let lp = LpTranslator::identity(partition_size);
    let sb = read_ext4_superblock(read_fn, &lp)?;

    log::info!(
        "[explorer] ext4: block_size={}, inode_size={}, bgd_entry_size={}",
        sb.block_size,
        sb.inode_size,
        sb.bgd_entry_size
    );

    let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, path)?;
    log::info!("[explorer] 找到 {} -> inode={}", path, inode_num);

    let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
    let mode = get_inode_mode(&inode_data);
    let size = get_inode_size(&inode_data);

    if is_dir_mode(mode) {
        return Err(format!("{} 是目录，不是文件", path));
    }

    log::info!(
        "[explorer] 读取 {} ({} bytes, mode=0x{:04X})",
        path,
        size,
        mode
    );
    let data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;

    Ok(data)
}

// =============================================================================
// 公共 API：通过 LP translator 从 super 分区读取文件
// =============================================================================

/// 通过 LP metadata 从 super 分区内的逻辑分区读取文件
///
/// 与 fs_shell 的 `cat <super_part>/<partition>/<path>` 完全相同的代码路径。
///
/// # 参数
/// - `super_read_fn`: 从 super 分区起始读取数据的函数
/// - `target_partition`: 目标逻辑分区名（如 "system_b"）
/// - `file_path`: ext4 内文件路径（如 "system/build.prop"）
///
/// # 返回
/// 文件内容字节
pub fn read_file_via_lp<F>(
    super_read_fn: &mut F,
    target_partition: &str,
    file_path: &str,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // Step 1: 从 super 读取 LP metadata
    let (partitions, _all_extents) = read_lp_metadata(super_read_fn)?;

    // Step 2: 查找目标逻辑分区
    let lp_part = partitions
        .iter()
        .find(|p| p.name == target_partition)
        .ok_or_else(|| format!("LP 中未找到分区: {}", target_partition))?;

    // 从 all_extents 中获取该分区的 extents
    let start = lp_part.first_extent_index as usize;
    let count = lp_part.num_extents as usize;
    let part_extents = if start + count <= _all_extents.len() {
        _all_extents[start..start + count].to_vec()
    } else {
        return Err(format!(
            "LP 分区 {} extents 索引越界: index={}, count={}, total={}",
            target_partition,
            start,
            count,
            _all_extents.len()
        ));
    };
    let part_size: u64 = part_extents.iter().map(|e| e.size).sum();

    log::info!(
        "[explorer] LP 分区 {}: {} extents, size=0x{:X}",
        target_partition,
        part_extents.len(),
        part_size
    );

    // Step 3: 创建 LP translator（从分区的 extents）
    let lp_translator = LpTranslator {
        extents: part_extents,
        total_size: part_size,
    };

    // Step 4: 读取 ext4 superblock
    let sb = read_ext4_superblock(super_read_fn, &lp_translator)?;

    log::info!(
        "[explorer] ext4: block_size={}, inode_size={}, bgd_entry_size={}",
        sb.block_size,
        sb.inode_size,
        sb.bgd_entry_size
    );

    // Step 5: 解析路径并读取文件
    let inode_num = resolve_path(super_read_fn, &lp_translator, &sb, ROOT_INODE, file_path)?;

    let inode_data = read_inode(super_read_fn, &lp_translator, &sb, inode_num)?;
    let mode = get_inode_mode(&inode_data);
    let size = get_inode_size(&inode_data);

    if is_dir_mode(mode) {
        return Err(format!("{} 是目录，不是文件", file_path));
    }

    log::info!(
        "[explorer] 找到 {}/{} -> inode={}, size={}",
        target_partition,
        file_path,
        inode_num,
        size
    );

    let data = read_inode_data(super_read_fn, &lp_translator, &sb, &inode_data)?;

    Ok(data)
}
