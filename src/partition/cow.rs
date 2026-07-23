//! Android Virtual A/B COW (Copy-on-Write) 格式解析器
//!
//! 支持两种 COW 格式：
//! 1. AOSP libsnapshot COW v2/v3（Android 12+，magic = 0xb5f0e642 / 0x94f09377）
//! 2. Linux Kernel dm-snapshot persistent（Android 11 VAB，magic = "SnAp" = 0x70416E53）
//!
//! 参考源码：
//! - AOSP: system/core/fs_mgr/libsnapshot/cow_format.h
//! - Linux Kernel: drivers/md/dm-snap-persistent.c

use log::{debug, info, warn};
use std::collections::HashMap;

// ============================================================================
// 格式检测
// ============================================================================

/// AOSP COW v2 magic
const COW_MAGIC_V2: u32 = 0xb5f0e642;
/// AOSP COW v3 magic
const COW_MAGIC_V3: u32 = 0x94f09377;
/// Linux dm-snapshot persistent magic "SnAp"
const SNAP_MAGIC: u32 = 0x70416e53;

/// 检测到的 COW 格式类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CowFormat {
    /// AOSP libsnapshot COW v2/v3
    AospCow,
    /// Linux Kernel dm-snapshot persistent
    DmSnapshotPersistent,
}

impl std::fmt::Display for CowFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CowFormat::AospCow => write!(f, "AOSP COW"),
            CowFormat::DmSnapshotPersistent => write!(f, "dm-snapshot persistent"),
        }
    }
}

/// 检测 COW 数据格式
/// 返回格式类型，如果不是已知 COW 格式返回 None
pub fn detect_cow_format(data: &[u8]) -> Option<CowFormat> {
    if data.len() < 16 {
        return None;
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    match magic {
        COW_MAGIC_V2 | COW_MAGIC_V3 => Some(CowFormat::AospCow),
        SNAP_MAGIC => Some(CowFormat::DmSnapshotPersistent),
        _ => None,
    }
}

// ============================================================================
// dm-snapshot persistent 格式
// ============================================================================

/// dm-snapshot persistent header (16 bytes)
///
/// Linux Kernel: drivers/md/dm-snap-persistent.c
/// ```c
/// struct disk_header {
///     __le32 magic;      // SNAP_MAGIC = 0x70416e53
///     __le32 valid;      // 1 = valid
///     __le32 version;    // 1
///     __le32 chunk_size; // chunk 大小，单位：扇区 (512B)
/// };
/// ```
#[derive(Debug, Clone)]
struct SnapHeader {
    magic: u32,
    valid: u32,
    version: u32,
    /// chunk 大小，单位：扇区 (512B)
    chunk_size: u32,
}

/// dm-snapshot persistent exception (16 bytes)
///
/// ```c
/// struct disk_exception {
///     __le64 old_chunk;  // origin 设备中的 chunk 编号
///     __le64 new_chunk;  // COW 设备中对应的 chunk 编号
/// };
/// ```
#[derive(Debug, Clone)]
struct SnapException {
    /// origin (base) 设备中的 chunk 编号
    old_chunk: u64,
    /// COW 设备中数据所在的 chunk 编号
    new_chunk: u64,
}

/// 解析 dm-snapshot persistent header
fn parse_snap_header(data: &[u8]) -> Result<SnapHeader, String> {
    if data.len() < 16 {
        return Err(format!("数据不足，dm-snapshot header 需要 16 字节"));
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if magic != SNAP_MAGIC {
        return Err(format!(
            "magic 不匹配: 期望 0x{:08X}, 实际 0x{:08X}",
            SNAP_MAGIC, magic
        ));
    }
    let valid = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let version = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let chunk_size = u32::from_le_bytes(data[12..16].try_into().unwrap());

    info!(
        "dm-snapshot persistent 格式: valid={}, version={}, chunk_size={} sectors ({} 字节)",
        valid,
        version,
        chunk_size,
        chunk_size * 512
    );

    if valid != 1 {
        warn!("快照标记为无效 (valid={})", valid);
    }
    if version != 1 {
        warn!("非预期版本号: {}（期望 1）", version);
    }

    Ok(SnapHeader {
        magic,
        valid,
        version,
        chunk_size,
    })
}

/// 读取所有 dm-snapshot persistent exceptions
///
/// 布局：
///   Chunk 0: Header
///   Chunk 1: Metadata Area 0 (exceptions)
///   Chunks 2..(1+e): Data Area 0
///   Chunk (2+e): Metadata Area 1
///   Chunks (3+e)..(2+2e): Data Area 1
///   ...
///
/// 其中 e = exceptions_per_area = (chunk_size * 512) / 16
///
/// Metadata Area N 的物理位置 = chunk (1 + N * (e + 1))
fn parse_snap_exceptions(
    cow_data: &[u8],
    header: &SnapHeader,
) -> Result<Vec<SnapException>, String> {
    let chunk_bytes = (header.chunk_size as u64) * 512;
    let exceptions_per_area = chunk_bytes / 16;

    info!(
        "chunk_bytes={}, exceptions_per_area={}",
        chunk_bytes, exceptions_per_area
    );

    let mut exceptions = Vec::new();
    let mut area = 0u32;

    loop {
        // Metadata Area N 的 chunk 位置
        let metadata_chunk = 1u64 + (exceptions_per_area + 1) * (area as u64);
        let metadata_offset = metadata_chunk * chunk_bytes;

        if metadata_offset as usize + chunk_bytes as usize > cow_data.len() {
            // 超出数据范围，停止
            break;
        }

        let metadata =
            &cow_data[metadata_offset as usize..(metadata_offset + chunk_bytes) as usize];

        let mut area_full = true;
        for i in 0..exceptions_per_area {
            let offset = (i * 16) as usize;
            let old_chunk = u64::from_le_bytes(metadata[offset..offset + 8].try_into().unwrap());
            let new_chunk =
                u64::from_le_bytes(metadata[offset + 8..offset + 16].try_into().unwrap());

            // new_chunk == 0 表示该 metadata area 到此结束
            //（chunk 0 是 header，不可能作为数据）
            if new_chunk == 0 {
                area_full = false;
                break;
            }

            exceptions.push(SnapException {
                old_chunk,
                new_chunk,
            });
        }

        if !area_full {
            break;
        }
        area += 1;

        // 安全限制：最多读取 1000 个 metadata area
        if area > 1000 {
            warn!("达到 metadata area 安全限制 (1000)，停止读取");
            break;
        }
    }

    info!("解析 {} 个 dm-snapshot exceptions", exceptions.len());
    Ok(exceptions)
}

/// 合并 dm-snapshot persistent 数据
///
/// 参数：
/// - cow_data: COW 分区完整数据
/// - base_data: base 分区数据（可选，如果为 None 则使用全零 buffer）
/// - base_size: base 分区总大小
///
/// 返回：(合并后的数据, 有效数据最大偏移)
fn merge_snap_data(
    cow_data: &[u8],
    base_data: Option<&[u8]>,
    base_size: u64,
) -> Result<(Vec<u8>, usize), String> {
    let header = parse_snap_header(cow_data)?;
    let exceptions = parse_snap_exceptions(cow_data, &header)?;

    let chunk_bytes = (header.chunk_size as u64) * 512;

    // 创建合并 buffer：优先使用 base 数据，否则全零
    let mut merged = if let Some(base) = base_data {
        base.to_vec()
    } else {
        vec![0u8; base_size as usize]
    };

    let mut max_offset = 0usize;
    let mut applied = 0usize;

    for ex in &exceptions {
        // 从 COW 的 new_chunk 位置读取数据
        let cow_offset = ex.new_chunk * chunk_bytes;
        let target_offset = (ex.old_chunk * chunk_bytes) as usize;
        let chunk_len = chunk_bytes as usize;

        if cow_offset as usize + chunk_len > cow_data.len() {
            warn!(
                "exception old={} new={}: COW 偏移 0x{:X} 超出范围",
                ex.old_chunk, ex.new_chunk, cow_offset
            );
            continue;
        }
        if target_offset + chunk_len > merged.len() {
            warn!(
                "exception old={} new={}: 目标偏移 0x{:X} 超出 buffer",
                ex.old_chunk, ex.new_chunk, target_offset
            );
            continue;
        }

        merged[target_offset..target_offset + chunk_len]
            .copy_from_slice(&cow_data[cow_offset as usize..cow_offset as usize + chunk_len]);

        let end = target_offset + chunk_len;
        if end > max_offset {
            max_offset = end;
        }
        applied += 1;
    }

    info!(
        "dm-snapshot 合并完成: {} / {} 个 exception 已应用, buffer 最大偏移 0x{:X}",
        applied,
        exceptions.len(),
        max_offset
    );

    if applied == 0 {
        return Err("dm-snapshot 中没有可应用的 exception".to_string());
    }

    Ok((merged, max_offset))
}

/// 只读取 dm-snapshot persistent 的 metadata areas，构建 exception 查询表
///
/// 相比 `merge_snap_data`，此函数不读取 COW 的 data areas，
/// 只读取 header 和 metadata areas，数据量通常为几百 KB 到几 MB，
/// 适合在设备上高效查询特定 chunk 的映射。
///
/// 参数：
/// - `read_cow`: 回调函数 `(offset, len) -> Result<Vec<u8>, String>`，用于读取 COW 分区
/// - `cow_size`: COW 分区总大小
///
/// 返回：`(chunk_size_sectors, HashMap<old_chunk, new_chunk>)`
pub fn snap_build_exception_table<R>(
    read_cow: &mut R,
    cow_size: u64,
) -> Result<(u32, HashMap<u64, u64>), String>
where
    R: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // 1. 读取 header（chunk 0，至少 16 字节）
    let header_read_len = std::cmp::min(cow_size, 4096);
    let header_data = read_cow(0, header_read_len)?;
    let header = parse_snap_header(&header_data)?;
    let chunk_bytes = (header.chunk_size as u64) * 512;

    // 2. 读取所有 metadata areas
    let exceptions_per_area = chunk_bytes / 16;
    let mut table = HashMap::new();
    let mut area = 0u32;

    loop {
        let metadata_chunk = 1u64 + (exceptions_per_area + 1) * (area as u64);
        let metadata_offset = metadata_chunk * chunk_bytes;

        if metadata_offset + chunk_bytes > cow_size {
            break;
        }

        let metadata = read_cow(metadata_offset, chunk_bytes)?;
        let mut area_full = true;

        for i in 0..exceptions_per_area {
            let offset = (i * 16) as usize;
            let old_chunk = u64::from_le_bytes(metadata[offset..offset + 8].try_into().unwrap());
            let new_chunk =
                u64::from_le_bytes(metadata[offset + 8..offset + 16].try_into().unwrap());

            if new_chunk == 0 {
                area_full = false;
                break;
            }

            table.insert(old_chunk, new_chunk);
        }

        if !area_full {
            break;
        }
        area += 1;

        if area > 1000 {
            warn!("达到 metadata area 安全限制 (1000)，停止读取");
            break;
        }
    }

    info!("读取 {} 个 dm-snapshot exceptions", table.len());
    Ok((header.chunk_size, table))
}

// ============================================================================
// AOSP COW v2/v3 格式（保留原有实现）
// ============================================================================

/// AOSP COW 操作类型
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u32)]
pub enum CowOpType {
    Copy = 0,
    Replace = 1,
    Zero = 2,
    Unknown(u32),
}

impl From<u32> for CowOpType {
    fn from(v: u32) -> Self {
        match v {
            0 => CowOpType::Copy,
            1 => CowOpType::Replace,
            2 => CowOpType::Zero,
            other => CowOpType::Unknown(other),
        }
    }
}

/// AOSP COW 头部
#[derive(Debug, Clone)]
pub struct AospCowHeader {
    pub version: u16,
    pub header_size: u16,
    pub block_size: u32,
    pub ops_offset: u64,
    pub ops_size: u64,
    pub num_ops: u32,
    pub total_size: u64,
    pub flags: u32,
}

/// AOSP COW 操作条目
#[derive(Debug, Clone)]
pub struct AospCowOp {
    pub op_type: CowOpType,
    pub source: u64,
    pub target: u64,
    pub data_length: u64,
}

fn parse_aosp_cow_header(data: &[u8]) -> Result<AospCowHeader, String> {
    const MIN_HEADER: usize = 48;
    if data.len() < MIN_HEADER {
        return Err(format!("数据不足，COW 头部至少需要 {} 字节", MIN_HEADER));
    }
    let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let header_version = u16::from_le_bytes(data[4..6].try_into().unwrap());
    let header_size = u16::from_le_bytes(data[6..8].try_into().unwrap());
    let block_size = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let ops_offset = u64::from_le_bytes(data[12..20].try_into().unwrap());
    let ops_size = u64::from_le_bytes(data[20..28].try_into().unwrap());
    let num_ops = u32::from_le_bytes(data[28..32].try_into().unwrap());
    let total_size = u64::from_le_bytes(data[32..40].try_into().unwrap());
    let flags = u32::from_le_bytes(data[40..44].try_into().unwrap());

    match magic {
        COW_MAGIC_V2 => {
            info!(
                "AOSP COW v2 格式: block_size={}, ops_offset=0x{:X}, num_ops={}, total_size=0x{:X}",
                block_size, ops_offset, num_ops, total_size
            );
        }
        COW_MAGIC_V3 => {
            info!(
                "AOSP COW v3 格式: block_size={}, ops_offset=0x{:X}, num_ops={}, total_size=0x{:X}, flags=0x{:X}",
                block_size, ops_offset, num_ops, total_size, flags
            );
        }
        _ => {
            return Err(format!("无效的 COW magic: 0x{:08X}", magic));
        }
    }

    Ok(AospCowHeader {
        version: header_version,
        header_size,
        block_size,
        ops_offset,
        ops_size,
        num_ops,
        total_size,
        flags,
    })
}

fn parse_aosp_cow_ops(data: &[u8], header: &AospCowHeader) -> Result<Vec<AospCowOp>, String> {
    let ops_start = header.ops_offset as usize;
    let ops_end = ops_start + header.ops_size as usize;

    if ops_start >= data.len() {
        return Err(format!(
            "ops_offset 0x{:X} 超出数据范围 {}",
            ops_start,
            data.len()
        ));
    }

    let available = if ops_end > data.len() {
        data.len()
    } else {
        ops_end
    };
    let ops_data = &data[ops_start..available];
    const OP_ENTRY_SIZE: usize = 28;

    let mut ops = Vec::with_capacity(header.num_ops as usize);
    let count = std::cmp::min(header.num_ops as usize, ops_data.len() / OP_ENTRY_SIZE);

    for i in 0..count {
        let offset = i * OP_ENTRY_SIZE;
        let op_type = u32::from_le_bytes(ops_data[offset..offset + 4].try_into().unwrap());
        let source = u64::from_le_bytes(ops_data[offset + 4..offset + 12].try_into().unwrap());
        let target = u64::from_le_bytes(ops_data[offset + 12..offset + 20].try_into().unwrap());
        let data_length =
            u64::from_le_bytes(ops_data[offset + 20..offset + 28].try_into().unwrap());

        let cow_type = CowOpType::from(op_type);
        // 跳过 Label(3) 和 Cluster(4)
        if cow_type == CowOpType::Unknown(3) || cow_type == CowOpType::Unknown(4) {
            continue;
        }

        ops.push(AospCowOp {
            op_type: cow_type,
            source,
            target,
            data_length,
        });
    }

    debug!("解析 {} 个 AOSP COW 操作", ops.len());
    Ok(ops)
}

fn merge_aosp_cow_data(cow_data: &[u8], base_size: u64) -> Result<(Vec<u8>, usize), String> {
    let header = parse_aosp_cow_header(cow_data)?;
    let ops = parse_aosp_cow_ops(cow_data, &header)?;

    let buffer_size = std::cmp::min(base_size, header.total_size) as usize;
    let mut merged = vec![0u8; buffer_size];

    let mut max_offset = 0usize;
    let mut replace_count = 0usize;

    for op in &ops {
        match op.op_type {
            CowOpType::Replace => {
                if op.source as usize + op.data_length as usize > cow_data.len() {
                    warn!(
                        "REPLACE source=0x{:X} len=0x{:X} 超出范围",
                        op.source, op.data_length
                    );
                    continue;
                }
                let target_offset = op.target as usize;
                let data_len = op.data_length as usize;
                if target_offset + data_len > buffer_size {
                    warn!(
                        "REPLACE target=0x{:X} len=0x{:X} 超出 buffer",
                        op.target, op.data_length
                    );
                    continue;
                }
                let src_start = op.source as usize;
                merged[target_offset..target_offset + data_len]
                    .copy_from_slice(&cow_data[src_start..src_start + data_len]);

                let end = target_offset + data_len;
                if end > max_offset {
                    max_offset = end;
                }
                replace_count += 1;
            }
            CowOpType::Zero => {}
            CowOpType::Copy => {
                debug!(
                    "COPY: source=0x{:X}, target=0x{:X}, len=0x{:X}（base 全零，跳过）",
                    op.source, op.target, op.data_length
                );
            }
            CowOpType::Unknown(n) => {
                if n == 5 {
                    debug!("XOR: 跳过");
                }
            }
        }
    }

    info!(
        "AOSP COW 合并完成: {} REPLACE, buffer 最大偏移 0x{:X}",
        replace_count, max_offset
    );

    if replace_count == 0 && max_offset == 0 {
        return Err("AOSP COW 中没有 REPLACE 操作".to_string());
    }

    Ok((merged, max_offset))
}

// ============================================================================
// 统一合并入口
// ============================================================================

/// 统一 COW 数据合并入口
///
/// 自动检测 COW 格式并执行相应的合并操作。
///
/// 参数：
/// - cow_data: COW 分区完整原始数据
/// - base_data: base 分区数据（可选）。对于 dm-snapshot 格式，
///   如果提供 base_data，会在此基础上应用 COW overlay；
///   对于 AOSP COW 格式，base_data 被忽略（直接用全零 buffer）。
/// - base_size: base 分区总大小
///
/// 返回：(合并后的数据, 有效数据最大偏移)
pub fn merge_cow_data(
    cow_data: &[u8],
    base_data: Option<&[u8]>,
    base_size: u64,
) -> Result<(Vec<u8>, usize), String> {
    match detect_cow_format(cow_data) {
        Some(CowFormat::DmSnapshotPersistent) => merge_snap_data(cow_data, base_data, base_size),
        Some(CowFormat::AospCow) => merge_aosp_cow_data(cow_data, base_size),
        None => {
            let hex: Vec<String> = cow_data[..std::cmp::min(16, cow_data.len())]
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect();
            Err(format!("未知的 COW 格式（头部: {}）", hex.join(" ")))
        }
    }
}

/// 检测数据是否全是零
pub fn is_all_zero(data: &[u8], check_size: usize) -> bool {
    let check_len = std::cmp::min(data.len(), check_size);
    data[..check_len].iter().all(|&b| b == 0)
}
