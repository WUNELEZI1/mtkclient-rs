//! liblp 数据结构定义
//!
//! 所有 AOSP liblp 元数据格式结构（packed，无填充）。

/// LpMetadataGeometry (4096 字节，对齐到 LP_SECTOR_SIZE)
///
/// AOSP 布局：
///   offset 0:  magic(u32) = 0x616C4467
///   offset 4:  struct_size(u32)
///   offset 8:  checksum(SHA256, 32 字节)
///   offset 40: metadata_max_size(u32)
///   offset 44: metadata_slot_count(u32)
///   offset 48: logical_block_size(u32)
#[derive(Debug, Clone)]
pub struct LpMetadataGeometry {
    pub logical_block_size: u32,
}

/// LpMetadataTableDescriptor (12 字节)
///
///   offset 0: offset(u32)   — 表位置，相对于 header 末尾
///   offset 4: num_entries(u32) — 条目数量
///   offset 8: entry_size(u32)  — 每个条目的字节数
#[derive(Debug, Clone)]
pub(crate) struct TableDescriptor {
    pub offset: u32,
    pub num_entries: u32,
    pub entry_size: u32,
}

/// LpMetadataHeader (256 字节)
///
/// AOSP 布局：
///   offset 0:  magic(u32) = 0x414C5030 ("0PLA")
///   offset 4:  major_version(u16)
///   offset 6:  minor_version(u16)
///   offset 8:  header_size(u32) = 256
///   offset 12: header_checksum(SHA256, 32 字节)
///   offset 44: tables_size(u32)
///   offset 48: tables_checksum(SHA256, 32 字节)
///   offset 80: partitions(TableDescriptor, 12 字节)
///   offset 92: extents(TableDescriptor, 12 字节)
///   offset 104: groups(TableDescriptor, 12 字节)
///   offset 116: block_devices(TableDescriptor, 12 字节)
///   offset 128: flags(u32)
///   offset 132: reserved(pad to 256)
#[derive(Debug, Clone)]
pub struct LpMetadataHeader {
    pub major_version: u16,
    pub minor_version: u16,
    pub header_size: u32,
    pub tables_size: u32,
    pub partitions: TableDescriptor,
    pub extents: TableDescriptor,
    pub groups: TableDescriptor,
    pub block_devices: TableDescriptor,
}

/// LpMetadataPartition (变长，默认 entry_size = 0x34 = 52 字节)
///
/// AOSP 布局：
///   offset 0:  name(char[36])
///   offset 36: attributes(u32)
///   offset 40: first_extent_index(u32)
///   offset 44: num_extents(u32)
///   offset 48: group_index(u32)
#[derive(Debug, Clone)]
pub struct LpMetadataPartition {
    pub name: String,
    pub attributes: u32,
    pub first_extent_index: u32,
    pub num_extents: u32,
}

/// 此分区名会附加当前槽位后缀（如 system → system_a）
pub const LP_PARTITION_ATTR_SLOT_SUFFIXED: u32 = 0x00000002;

impl LpMetadataPartition {
    /// 是否带有槽位后缀（A/B 分区）
    pub fn is_slot_suffixed(&self) -> bool {
        (self.attributes & LP_PARTITION_ATTR_SLOT_SUFFIXED) != 0
    }
}

/// LpMetadataExtent (变长，默认 entry_size = 0x18 = 24 字节)
///
/// AOSP 布局：
///   offset 0:  num_sectors(u64)
///   offset 8:  target_type(u32)  — 0=LINEAR, 1=ZERO
///   offset 12: target_data(u64)   — LINEAR: 物理扇区偏移
///   offset 20: target_source(u32) — LINEAR: block_devices 表索引
#[derive(Debug, Clone)]
pub struct LpMetadataExtent {
    pub num_sectors: u64,
    pub target_data: u64,
}
