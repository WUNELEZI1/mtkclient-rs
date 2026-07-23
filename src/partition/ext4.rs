//! 轻量级 ext4 文件系统解析器
//!
//! 用于在分区原始数据中精确定位 /system/build.prop 文件，
//! 无需挂载文件系统。只读取必要的元数据块（superblock + bgd + inode + extents），
//! 然后直接读取 build.prop 的数据块，通常仅需读取 2-4 个 4KB 块。
//!
//! 支持 extent-based 文件（ext4 默认，i_flags & 0x80000）。

use log::{info, trace};

// ============================================================================
// ext4 常量
// ============================================================================

/// ext4 superblock 偏移（从分区起始 + 1024 字节）
const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
/// ext4 magic
const EXT4_MAGIC: u16 = 0xEF53;
/// Inode 使用 extents 标志
const EXT4_INODE_FLAG_EXTENTS: u32 = 0x0008_0000;
/// 普通文件类型
const EXT4_FT_REG_FILE: u8 = 1;
/// 目录类型
const EXT4_FT_DIR: u8 = 2;
/// Extent header magic
const EXTENT_HEADER_MAGIC: u16 = 0xF30A;
/// Root inode 编号（固定为 2）
const ROOT_INODE: u32 = 2;

// ============================================================================
// ext4 Superblock 解析
// ============================================================================

/// ext4 superblock 关键字段
#[derive(Debug, Clone)]
struct Superblock {
    block_size: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: u16,
    inodes_count: u32,
}

fn parse_superblock(data: &[u8]) -> Result<Superblock, String> {
    if data.len() < 1024 {
        return Err("superblock 数据不足 1024 字节".to_string());
    }
    let magic = u16::from_le_bytes(data[56..58].try_into().unwrap());
    if magic != EXT4_MAGIC {
        return Err(format!(
            "ext4 magic 不匹配: 0x{:04X} (期望 0x{:04X})",
            magic, EXT4_MAGIC
        ));
    }

    let inodes_count = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let first_data_block = u32::from_le_bytes(data[20..24].try_into().unwrap());
    let log_block_size = u32::from_le_bytes(data[24..28].try_into().unwrap());
    let block_size = 1024u32 << log_block_size;
    let blocks_per_group = u32::from_le_bytes(data[32..36].try_into().unwrap());
    let inodes_per_group = u32::from_le_bytes(data[40..44].try_into().unwrap());
    let inode_size = u16::from_le_bytes(data[88..90].try_into().unwrap());

    info!(
        "ext4: block_size={}, inodes={}, blocks_per_group={}, inode_size={}",
        block_size, inodes_count, blocks_per_group, inode_size
    );

    Ok(Superblock {
        block_size,
        first_data_block,
        blocks_per_group,
        inodes_per_group,
        inode_size: if inode_size == 0 { 128 } else { inode_size },
        inodes_count,
    })
}

// ============================================================================
// Block Group Descriptor
// ============================================================================

/// 块组描述符
#[derive(Debug, Clone)]
struct BlockGroupDescriptor {
    inode_table: u32,
}

fn parse_bgd(data: &[u8]) -> Result<BlockGroupDescriptor, String> {
    if data.len() < 12 {
        return Err("bgd 数据不足".to_string());
    }
    let inode_table = u32::from_le_bytes(data[8..12].try_into().unwrap());
    Ok(BlockGroupDescriptor { inode_table })
}

// ============================================================================
// Inode
// ============================================================================

/// Inode 结构
#[derive(Debug, Clone)]
struct Inode {
    mode: u16,
    size: u32,
    blocks: u32,
    flags: u32,
    data: Vec<u8>,
    uses_extents: bool,
}

fn parse_inode(data: &[u8], _inode_size: u16) -> Inode {
    let mode = u16::from_le_bytes(data[0..2].try_into().unwrap());
    let size = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let blocks = u32::from_le_bytes(data[28..32].try_into().unwrap());
    let flags = u32::from_le_bytes(data[32..36].try_into().unwrap());
    Inode {
        mode,
        size,
        blocks,
        flags,
        data: data.to_vec(),
        uses_extents: (flags & EXT4_INODE_FLAG_EXTENTS) != 0,
    }
}

// ============================================================================
// Extent
// ============================================================================

/// Extent entry
#[derive(Debug, Clone)]
struct Extent {
    /// 逻辑块号
    lblock: u32,
    /// 连续块数
    len: u16,
    /// 物理块号（高 16 位 + 低 32 位）
    pblock: u64,
}

/// 递归解析 extent 树节点（支持 depth=0 叶子节点和 depth>=1 索引节点）
fn parse_extents_recursive<F>(
    read_fn: &mut F,
    sb: &Superblock,
    i_block_data: &[u8],
) -> Result<Vec<Extent>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let entries = u16::from_le_bytes(i_block_data[2..4].try_into().unwrap());
    let depth = u16::from_le_bytes(i_block_data[6..8].try_into().unwrap());

    let mut extents = Vec::new();

    for i in 0..entries {
        let offset = 12 + (i as usize) * 12;
        if offset + 12 > i_block_data.len() {
            break;
        }
        let lblock = u32::from_le_bytes(i_block_data[offset..offset + 4].try_into().unwrap());
        let len = u16::from_le_bytes(i_block_data[offset + 4..offset + 6].try_into().unwrap());
        let start_hi = u16::from_le_bytes(i_block_data[offset + 6..offset + 8].try_into().unwrap());
        let start_lo = u32::from_le_bytes(i_block_data[offset + 8..offset + 12].try_into().unwrap());
        let pblock = ((start_hi as u64) << 32) | (start_lo as u64);

        if depth == 0 {
            // 叶子节点：直接收集 extent
            info!(
                "  extent[{}]: lblock={}, len={}, pblock=0x{:X} (start_hi=0x{:04X}, start_lo=0x{:08X})",
                i, lblock, len, pblock, start_hi, start_lo
            );
            extents.push(Extent {
                lblock,
                len,
                pblock,
            });
        } else {
            // 索引节点：读取子块并递归解析
            let block_size = sb.block_size as u64;
            let child_offset = pblock * block_size;
            let child_data = read_fn(child_offset, block_size)?;
            let child_extents = parse_extents_recursive(read_fn, sb, &child_data)?;
            extents.extend(child_extents);
        }
    }

    Ok(extents)
}

/// 解析 inode 中的 extent tree
fn parse_extents<F>(read_fn: &mut F, inode: &Inode, sb: &Superblock) -> Result<Vec<Extent>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    if !inode.uses_extents {
        return Err("inode 不使用 extents（可能是旧版 ext 格式）".to_string());
    }

    // i_block 在 inode 中的偏移为 40，长度 60 字节
    let i_block = &inode.data[40..40 + 60];
    let magic = u16::from_le_bytes(i_block[0..2].try_into().unwrap());
    if magic != EXTENT_HEADER_MAGIC {
        return Err(format!(
            "extent header magic 不匹配: 0x{:04X} (期望 0x{:04X})",
            magic, EXTENT_HEADER_MAGIC
        ));
    }

    let entries = u16::from_le_bytes(i_block[2..4].try_into().unwrap());
    let depth = u16::from_le_bytes(i_block[6..8].try_into().unwrap());

    info!("extent header: entries={}, depth={}", entries, depth);

    parse_extents_recursive(read_fn, sb, i_block)
}

/// 计算文件数据在分区中的物理偏移范围
fn get_file_physical_ranges(extents: &[Extent], block_size: u32) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    for ext in extents {
        let start = ext.pblock * (block_size as u64);
        let end = (ext.pblock + ext.len as u64) * (block_size as u64);
        info!(
            "  file range: pblock=0x{:X} * block_size={} => offset=0x{:X}..0x{:X} ({} bytes)",
            ext.pblock,
            block_size,
            start,
            end,
            end - start
        );
        ranges.push((start, end));
    }
    ranges
}

// ============================================================================
// Directory Entry
// ============================================================================

/// 目录项
#[derive(Debug, Clone)]
struct DirEntry {
    inode: u32,
    name: String,
    file_type: u8,
}

/// 解析目录数据中的条目
fn parse_directory(data: &[u8]) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset + 8 < data.len() {
        let inode = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        let rec_len = u16::from_le_bytes(data[offset + 4..offset + 6].try_into().unwrap());
        if rec_len == 0 || rec_len as usize > data.len() - offset {
            break;
        }
        let name_len = data[offset + 6] as usize;
        let file_type = data[offset + 7];
        let name = String::from_utf8_lossy(&data[offset + 8..offset + 8 + name_len]).to_string();

        entries.push(DirEntry {
            inode,
            name,
            file_type,
        });
        offset += rec_len as usize;
    }

    entries
}

// ============================================================================
// 核心解析器
// ============================================================================

/// 在分区原始数据中查找 /system/build.prop 文件并读取其内容
///
/// 参数：
/// - `read_fn`: 读取分区数据的回调函数 `(addr, len) -> Result<Vec<u8>, String>`
/// - `partition_size`: 分区总大小（用于范围校验）
///
/// 返回：`(build.prop 文件内容, 物理偏移范围列表, 块大小)`
/// 物理偏移范围列表用于定向 COW 合并。
pub fn read_buildprop_from_partition<F>(
    read_fn: &mut F,
    partition_size: u64,
) -> Result<(Vec<u8>, Vec<(u64, u64)>, u32), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // 1. 读取 superblock (1024 bytes)
    let sb_offset = EXT4_SUPERBLOCK_OFFSET;
    if sb_offset + 1024 > partition_size {
        return Err("分区太小，无法读取 superblock".to_string());
    }
    let sb_data = read_fn(sb_offset, 1024)?;
    let sb = parse_superblock(&sb_data)?;

    // 2. 读取 block group 0 的 BGD（诊断信息，实际 inode 读取由 read_bgd_for_inode 按需定位）
    let bgd_offset = if sb.block_size > 1024 {
        sb.block_size as u64
    } else {
        2048
    };
    let bgd_data = read_fn(bgd_offset, 64)?;
    let _bgd0 = parse_bgd(&bgd_data)?;
    info!("bgd[0]: inode_table_block=0x{:X}", _bgd0.inode_table);

    // 3. 读取 root inode (#2)
    let root_inode = read_inode(read_fn, &sb, ROOT_INODE)?;
    trace!(
        "root inode: mode=0x{:04X}, size={}, extents={}",
        root_inode.mode, root_inode.size, root_inode.uses_extents
    );

    // 4. 读取 root 目录内容
    let root_data = read_inode_data(read_fn, &sb, &root_inode)?;
    let root_entries = parse_directory(&root_data);
    trace!("root directory: {} entries", root_entries.len());

    // 5. 查找 "system" 目录
    let system_dir = root_entries
        .iter()
        .find(|e| e.name == "system" && e.file_type == EXT4_FT_DIR)
        .ok_or_else(|| "未找到 /system 目录".to_string())?;
    info!("找到 /system 目录 (inode={})", system_dir.inode);

    // 6. 读取 system 目录内容
    let system_inode = read_inode(read_fn, &sb, system_dir.inode)?;
    let system_data = read_inode_data(read_fn, &sb, &system_inode)?;
    let system_entries = parse_directory(&system_data);
    trace!("/system directory: {} entries", system_entries.len());

    // 7. 查找 "build.prop" 文件
    let build_prop = system_entries
        .iter()
        .find(|e| e.name == "build.prop" && e.file_type == EXT4_FT_REG_FILE)
        .ok_or_else(|| "未找到 /system/build.prop".to_string())?;
    info!("找到 /system/build.prop (inode={})", build_prop.inode);

    // 同时列出 system 目录中所有文件（调试用）
    info!("/system 目录条目:");
    for e in &system_entries {
        if e.inode > 0 {
            info!(
                "  inode={:>5} type={} name={}",
                e.inode, e.file_type, e.name
            );
        }
    }

    // 8. 读取 build.prop 的 inode
    let bp_inode = read_inode(read_fn, &sb, build_prop.inode)?;
    info!(
        "build.prop: size={} bytes, extents={}",
        bp_inode.size, bp_inode.uses_extents
    );
    // 打印 inode 数据的 hex dump（前 64 字节）
    let inode_hex: Vec<String> = bp_inode.data[..64.min(bp_inode.data.len())]
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect();
    info!("build.prop inode raw (前 64 字节): {}", inode_hex.join(" "));

    // 9. 读取文件数据
    let bp_data = read_inode_data(read_fn, &sb, &bp_inode)?;

    // 获取物理块范围（用于定向 COW 合并）
    let bp_extents = parse_extents(read_fn, &bp_inode, &sb)?;
    let bp_ranges = get_file_physical_ranges(&bp_extents, sb.block_size);

    info!("成功读取 build.prop: {} 字节", bp_data.len());

    // 打印数据前 64 字节的 hex dump
    let data_hex: Vec<String> = bp_data[..64.min(bp_data.len())]
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect();
    info!("build.prop 数据前 64 字节: {}", data_hex.join(" "));

    // 尝试查找 etc/apns-conf.xml 对比
    if let Some(etc_entry) = system_entries
        .iter()
        .find(|e| e.name == "etc" && e.file_type == EXT4_FT_DIR)
    {
        info!(
            "找到 /system/etc (inode={})，尝试查找 apns-conf.xml...",
            etc_entry.inode
        );
        if let Ok(etc_inode) = read_inode(read_fn, &sb, etc_entry.inode) {
            if let Ok(etc_data) = read_inode_data(read_fn, &sb, &etc_inode) {
                let etc_entries = parse_directory(&etc_data);
                if let Some(apn) = etc_entries
                    .iter()
                    .find(|e| e.name == "apns-conf.xml" && e.file_type == EXT4_FT_REG_FILE)
                {
                    info!("找到 /system/etc/apns-conf.xml (inode={})", apn.inode);
                    if let Ok(apn_inode) = read_inode(read_fn, &sb, apn.inode) {
                        info!(
                            "apns-conf.xml: size={} bytes, extents={}",
                            apn_inode.size, apn_inode.uses_extents
                        );
                        if let Ok(apn_data) = read_inode_data(read_fn, &sb, &apn_inode) {
                            let apn_hex: Vec<String> = apn_data[..64.min(apn_data.len())]
                                .iter()
                                .map(|b| format!("{:02X}", b))
                                .collect();
                            info!("apns-conf.xml 数据前 64 字节: {}", apn_hex.join(" "));
                        }
                    }
                }
            }
        }
    }

    Ok((bp_data, bp_ranges, sb.block_size))
}

/// 根据 inode 编号计算其所属的 block group，并读取对应的 BGD
fn read_bgd_for_inode<F>(
    read_fn: &mut F,
    sb: &Superblock,
    inode_num: u32,
) -> Result<BlockGroupDescriptor, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let group = (inode_num - 1) / sb.inodes_per_group;
    let bgd_table_offset = if sb.block_size > 1024 {
        sb.block_size as u64 // block 1
    } else {
        2048 // block 2 (block_size=1024 时 superblock 占一整个 block)
    };
    let bgd_entry_offset = bgd_table_offset + (group as u64) * 64; // 64B per desc
    info!(
        "read_bgd_for_inode: inode={} -> group={}, bgd_entry_offset=0x{:X}",
        inode_num, group, bgd_entry_offset
    );
    let bgd_data = read_fn(bgd_entry_offset, 64)?;
    parse_bgd(&bgd_data)
}

/// 读取指定 inode
fn read_inode<F>(read_fn: &mut F, sb: &Superblock, inode_num: u32) -> Result<Inode, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // 根据 inode 编号找到它所属 block group 的 BGD
    let bgd = read_bgd_for_inode(read_fn, sb, inode_num)?;

    let inode_table_offset = (bgd.inode_table as u64) * (sb.block_size as u64);
    let inode_index = (inode_num - 1) as u64;
    let inode_offset = inode_table_offset + inode_index * (sb.inode_size as u64);

    info!(
        "read_inode #{}: group={}, bgd.inode_table=0x{:X}, inode_table_offset=0x{:X}, inode_offset=0x{:X}",
        inode_num,
        (inode_num - 1) / sb.inodes_per_group,
        bgd.inode_table,
        inode_table_offset,
        inode_offset
    );

    let inode_data = read_fn(inode_offset, sb.inode_size as u64)?;
    Ok(parse_inode(&inode_data, sb.inode_size))
}

/// 读取 inode 的所有文件数据
fn read_inode_data<F>(read_fn: &mut F, sb: &Superblock, inode: &Inode) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let extents = parse_extents(read_fn, inode, sb)?;
    let ranges = get_file_physical_ranges(&extents, sb.block_size);

    let mut data = Vec::new();
    for (start, end) in ranges {
        let len = end - start;
        let chunk = read_fn(start, len)?;
        data.extend_from_slice(&chunk);
    }

    // Trim 到实际文件大小
    let actual_size = inode.size as usize;
    if data.len() > actual_size {
        data.truncate(actual_size);
    }

    Ok(data)
}

// ============================================================================
// 辅助：检测分区是否是 ext4
// ============================================================================

/// 快速检测分区数据是否是 ext4 格式
pub fn is_ext4(data: &[u8]) -> bool {
    if data.len() < 1024 + 58 {
        return false;
    }
    let magic = u16::from_le_bytes(data[56..58].try_into().unwrap());
    magic == EXT4_MAGIC
}
