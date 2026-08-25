use super::lp::LpTranslator;

const EXT4_MAGIC: u16 = 0xEF53;
const EXTENT_HEADER_MAGIC: u16 = 0xF30A;
pub(crate) const ROOT_INODE: u32 = 2;
pub(crate) const EXT4_INODE_FLAG_EXTENTS: u32 = 0x0008_0000;

#[derive(Debug, Clone)]
pub(crate) struct Ext4Superblock {
    pub(crate) block_size: u32,
    pub(crate) inodes_per_group: u32,
    pub(crate) inode_size: u16,
    pub(crate) bgd_entry_size: u16, // 32 或 64，取决于 INCOMPAT_64BIT
}

#[derive(Debug, Clone)]
pub(crate) struct DirEntry {
    pub(crate) inode: u32,
    pub(crate) file_type: u8, // 1=file, 2=dir, 7=symlink
    pub(crate) name: String,
}

pub(crate) fn read_ext4_superblock<F>(
    read_fn: &mut F,
    lp: &LpTranslator,
) -> Result<Ext4Superblock, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let abs_off = lp.logical_to_abs(1024)?;
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
pub(crate) fn read_inode<F>(
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
    let bgd_abs = lp.logical_to_abs(bgd_entry_offset)?;
    let bgd_data = read_fn(bgd_abs, sb.bgd_entry_size as u64)?;
    if bgd_data.len() < 12 {
        return Err(format!(
            "短读：期望 12 字节（BGD），实际 {}",
            bgd_data.len()
        ));
    }
    let inode_table_block = u32::from_le_bytes(bgd_data[8..12].try_into().unwrap());

    // 计算 inode 在分区内的逻辑偏移
    let inode_logical_offset = inode_table_block as u64 * sb.block_size as u64
        + (inode_num - 1) as u64 * sb.inode_size as u64;

    // 通过 LP 翻译
    let abs_offset = lp.logical_to_abs(inode_logical_offset)?;
    let inode_data = read_fn(abs_offset, sb.inode_size as u64)?;
    Ok(inode_data)
}

/// 获取 inode 文件大小 (i_size_lo | i_size_hi << 32)
pub(crate) fn get_inode_size(inode_data: &[u8]) -> u64 {
    if inode_data.len() < 8 {
        return 0;
    }
    let size_lo = u32::from_le_bytes(inode_data[4..8].try_into().unwrap());
    let size_hi = if inode_data.len() >= 112 {
        u32::from_le_bytes(inode_data[108..112].try_into().unwrap())
    } else {
        0
    };
    (size_lo as u64) | ((size_hi as u64) << 32)
}

/// 获取 inode mode (u16 @ offset 0)
pub(crate) fn get_inode_mode(inode_data: &[u8]) -> u16 {
    if inode_data.len() < 2 {
        return 0;
    }
    u16::from_le_bytes(inode_data[0..2].try_into().unwrap())
}

/// 获取 inode flags (u32 @ offset 32)
pub(crate) fn get_inode_flags(inode_data: &[u8]) -> u32 {
    if inode_data.len() < 36 {
        return 0;
    }
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
pub(crate) fn read_inode_data<F>(
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

    // i_block 区域至少需要 2 字节以读取 extent header magic（offset 40..42）
    if inode_data.len() < 42 {
        return Err(format!(
            "短读：inode 数据不足 42 字节，实际 {}",
            inode_data.len()
        ));
    }

    // symlink 内联数据：目标路径直接存储在 i_block 区域（最大 60 字节）
    if is_symlink_mode(mode) && (flags & EXT4_INODE_FLAG_EXTENTS) == 0 {
        let i_block = &inode_data[40..100.min(inode_data.len())];
        let symlink_len = (file_size as usize).min(60).min(i_block.len());
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
pub(crate) fn parse_dir(data: &[u8]) -> Vec<DirEntry> {
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
pub(crate) fn resolve_path<F>(
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
