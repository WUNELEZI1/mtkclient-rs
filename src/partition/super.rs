//! Android Dynamic Partition (liblp) 解析器
//!
//! 严格按照 AOSP system/core/fs_mgr/liblp/include/liblp/metadata_format.h 标准实现。
//!
//! 数据布局（super 分区或 metadata 分区）：
//!   0x0000  reserved (LP_PARTITION_RESERVED_BYTES = 4096)
//!   0x1000  primary Geometry   (LpMetadataGeometry, 固定 4096 字节)
//!   0x2000  backup Geometry    (LpMetadataGeometry, 固定 4096 字节)
//!   0x3000  primary Metadata   (Header + Tables)
//!   ...     backup Metadata(s)
//!   ...     logical partition data
//!
//! Metadata 内部：
//!   Header (固定 256 字节，含 4 个 LpMetadataTableDescriptor)
//!   Partition table
//!   Extent table
//!   Group table
//!   Block device table
//!
//! 注意：MTK Virtual A/B 设备（如 MT6768）可能将 metadata 存储在独立的
//! metadata 分区（32MB）中，而非 super 分区头部。此时 super 分区可能全零。

use log::{debug, trace, warn};

// ============================================================================
// AOSP 标准 Magic 值
// ============================================================================

/// LP_METADATA_GEOMETRY_MAGIC = 0x674C4164 ("gDla" in LE)
const GEOMETRY_MAGIC: u32 = 0x674C4164;
/// LP_METADATA_HEADER_MAGIC = 0x414C5030 ("0PLA" in LE)
const HEADER_MAGIC: u32 = 0x414C5030;

// ============================================================================
// AOSP 标准数据结构（packed，无填充）
// ============================================================================

/// LpMetadataGeometry (4096 字节，对齐到 LP_SECTOR_SIZE)
///
/// AOSP 布局：
///   offset 0:  magic(u32) = 0x674C4164
///   offset 4:  struct_size(u32)
///   offset 8:  checksum(SHA256, 32 字节)
///   offset 40: metadata_max_size(u32)
///   offset 44: metadata_slot_count(u32)
///   offset 48: logical_block_size(u32)
#[derive(Debug, Clone)]
pub struct LpMetadataGeometry {
    pub magic: u32,
    pub struct_size: u32,
    pub metadata_max_size: u32,
    pub metadata_slot_count: u32,
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
    pub magic: u32,
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
    pub group_index: u32,
}

/// AOSP LP_PARTITION_ATTR 位定义
pub const LP_PARTITION_ATTR_READONLY: u32 = 0x00000001;
/// 此分区名会附加当前槽位后缀（如 system → system_a）
pub const LP_PARTITION_ATTR_SLOT_SUFFIXED: u32 = 0x00000002;
/// 此分区在所有槽位中唯一（不需要后缀）
pub const LP_PARTITION_ATTR_UPDATED: u32 = 0x00000004;

impl LpMetadataPartition {
    /// 是否带有槽位后缀（A/B 分区）
    pub fn is_slot_suffixed(&self) -> bool {
        (self.attributes & LP_PARTITION_ATTR_SLOT_SUFFIXED) != 0
    }

    /// 提取基础名称（去掉 _a/_b 后缀）
    pub fn base_name(&self) -> &str {
        let name = self.name.as_str();
        if let Some(base) = name.strip_suffix("_a") {
            base
        } else if let Some(base) = name.strip_suffix("_b") {
            base
        } else {
            name
        }
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
    pub target_type: u32,
    pub target_data: u64,
    pub target_source: u32,
}

/// LpMetadataPartitionGroup (变长，默认 entry_size = 0x30 = 48 字节)
///
/// AOSP 布局：
///   offset 0:  name(char[36])
///   offset 36: flags(u32)
///   offset 40: maximum_size(u64)
#[derive(Debug, Clone)]
pub struct LpMetadataGroup {
    pub name: String,
    pub maximum_size: u64,
    pub flags: u32,
}

/// 解析后的 Super 分区元数据
#[derive(Debug, Clone)]
pub struct SuperMetadata {
    pub geometry: LpMetadataGeometry,
    pub header: LpMetadataHeader,
    pub partitions: Vec<LpMetadataPartition>,
    pub extents: Vec<LpMetadataExtent>,
    pub groups: Vec<LpMetadataGroup>,
    /// 逻辑块大小（扇区大小，默认 512 字节）
    pub block_size: u32,
}

// ============================================================================
// SuperMetadata 主解析逻辑
// ============================================================================

impl SuperMetadata {
    /// 从 super/metadata 分区原始数据中解析元数据
    ///
    /// 策略：
    /// 1. 扫描 "0PLA" (LP_METADATA_HEADER_MAGIC = 0x414C5030)
    /// 2. 向前回溯找到 Geometry (magic = 0x674C4164)
    /// 3. 如果找不到 Geometry，直接从 Header 位置解析
    /// 4. 按 TableDescriptor 定位各表，解析 partition/extent/group
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let max_scan = std::cmp::min(data.len(), 0x80000); // 扫描前 512KB

        // 步骤 1：扫描 Header magic "0PLA"
        let header_offset = Self::find_header_magic(data, max_scan)?;
        debug!(
            "找到 LP_METADATA_HEADER_MAGIC at offset 0x{:04X}",
            header_offset
        );

        // 步骤 2：尝试找 Geometry
        let geometry = Self::find_geometry(data, header_offset, max_scan);

        // 步骤 3：解析 Header（含 TableDescriptor）
        let header = Self::parse_header(data, header_offset)?;
        trace!(
            "Header: version={}.{}, header_size={}, tables_size={}",
            header.major_version, header.minor_version, header.header_size, header.tables_size
        );
        trace!(
            "  partitions: offset={}, count={}, entry_size={}",
            header.partitions.offset, header.partitions.num_entries, header.partitions.entry_size
        );
        trace!(
            "  extents:    offset={}, count={}, entry_size={}",
            header.extents.offset, header.extents.num_entries, header.extents.entry_size
        );
        trace!(
            "  groups:     offset={}, count={}, entry_size={}",
            header.groups.offset, header.groups.num_entries, header.groups.entry_size
        );
        trace!(
            "  block_devs: offset={}, count={}, entry_size={}",
            header.block_devices.offset,
            header.block_devices.num_entries,
            header.block_devices.entry_size
        );

        // 步骤 4：按 TableDescriptor 解析各表
        // 表位置 = header_offset + header_size + descriptor.offset
        let tables_base = header_offset + header.header_size as usize;

        let partitions = Self::parse_partitions(
            data,
            tables_base + header.partitions.offset as usize,
            header.partitions.num_entries,
            header.partitions.entry_size as usize,
        )?;
        debug!("解析 {} 个 logical partition", partitions.len());

        let extents = Self::parse_extents(
            data,
            tables_base + header.extents.offset as usize,
            header.extents.num_entries,
            header.extents.entry_size as usize,
        )?;
        debug!("解析 {} 个 extent", extents.len());

        let groups = Self::parse_groups(
            data,
            tables_base + header.groups.offset as usize,
            header.groups.num_entries,
            header.groups.entry_size as usize,
        );
        debug!("解析 {} 个 group", groups.len());

        // 确定 block_size：优先从 geometry 读取，否则默认 512
        let block_size = geometry
            .as_ref()
            .map(|g| g.logical_block_size)
            .unwrap_or(512);

        // 构造默认 geometry（如果没找到）
        let geo = geometry.unwrap_or(LpMetadataGeometry {
            magic: GEOMETRY_MAGIC,
            struct_size: 0,
            metadata_max_size: 0,
            metadata_slot_count: 1,
            logical_block_size: block_size,
        });

        Ok(SuperMetadata {
            geometry: geo,
            header,
            partitions,
            extents,
            groups,
            block_size,
        })
    }

    // ========================================================================
    // 扫描 magic
    // ========================================================================

    /// 扫描 "0PLA" Header magic
    fn find_header_magic(data: &[u8], max_scan: usize) -> Result<usize, String> {
        for offset in (0..max_scan.saturating_sub(256)).step_by(512) {
            if offset + 4 > data.len() {
                break;
            }
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic == HEADER_MAGIC {
                return Ok(offset);
            }
        }
        // 也尝试逐字节扫描（某些偏移可能不对齐到 512）
        for offset in 0..std::cmp::min(data.len().saturating_sub(256), max_scan) {
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic == HEADER_MAGIC {
                return Ok(offset);
            }
        }
        Err(format!(
            "未找到 LP_METADATA_HEADER_MAGIC (0x{:08X})，扫描范围 0..0x{:X}",
            HEADER_MAGIC, max_scan
        ))
    }

    /// 从 header_offset 向前回溯查找 Geometry
    fn find_geometry(
        data: &[u8],
        header_offset: usize,
        _max_scan: usize,
    ) -> Option<LpMetadataGeometry> {
        // Geometry 通常在 header 之前的 0x1000 倍数位置
        // 扫描 header_offset 之前的区域
        let scan_start = header_offset.saturating_sub(0x40000);
        for offset in (scan_start..header_offset).rev().step_by(512) {
            if offset + 52 > data.len() {
                continue;
            }
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic == GEOMETRY_MAGIC {
                debug!("找到 LP_METADATA_GEOMETRY_MAGIC at offset 0x{:04X}", offset);
                let struct_size =
                    u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                let metadata_max_size = if offset + 44 <= data.len() {
                    u32::from_le_bytes(data[offset + 40..offset + 44].try_into().unwrap())
                } else {
                    0
                };
                let metadata_slot_count = if offset + 48 <= data.len() {
                    u32::from_le_bytes(data[offset + 44..offset + 48].try_into().unwrap())
                } else {
                    1
                };
                let logical_block_size = if offset + 52 <= data.len() {
                    u32::from_le_bytes(data[offset + 48..offset + 52].try_into().unwrap())
                } else {
                    512
                };
                return Some(LpMetadataGeometry {
                    magic,
                    struct_size,
                    metadata_max_size,
                    metadata_slot_count,
                    logical_block_size,
                });
            }
        }
        warn!("未找到 LP_METADATA_GEOMETRY_MAGIC，使用默认值");
        None
    }

    // ========================================================================
    // 解析 Header
    // ========================================================================

    fn parse_header(data: &[u8], offset: usize) -> Result<LpMetadataHeader, String> {
        // Header 最少需要 128 字节才能读取 4 个 TableDescriptor
        const MIN_HEADER_SIZE: usize = 128;
        if offset + MIN_HEADER_SIZE > data.len() {
            return Err(format!(
                "数据不足，Header 需要 {} 字节，可用 {} 字节",
                MIN_HEADER_SIZE,
                data.len().saturating_sub(offset)
            ));
        }

        let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        if magic != HEADER_MAGIC {
            return Err(format!(
                "Header magic 不匹配: 期望 0x{:08X}, 实际 0x{:08X}",
                HEADER_MAGIC, magic
            ));
        }

        let major_version = u16::from_le_bytes(data[offset + 4..offset + 6].try_into().unwrap());
        let minor_version = u16::from_le_bytes(data[offset + 6..offset + 8].try_into().unwrap());
        let header_size = u32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap());

        // tables_size 在 offset 44 (跳过 32 字节 SHA256 header_checksum)
        let tables_size = u32::from_le_bytes(data[offset + 44..offset + 48].try_into().unwrap());

        // 4 个 TableDescriptor 从 offset 80 开始，每个 12 字节
        let partitions = Self::read_table_descriptor(data, offset + 80)?;
        let extents = Self::read_table_descriptor(data, offset + 92)?;
        let groups = Self::read_table_descriptor(data, offset + 104)?;
        let block_devices = Self::read_table_descriptor(data, offset + 116)?;

        Ok(LpMetadataHeader {
            magic,
            major_version,
            minor_version,
            header_size,
            tables_size,
            partitions,
            extents,
            groups,
            block_devices,
        })
    }

    fn read_table_descriptor(data: &[u8], offset: usize) -> Result<TableDescriptor, String> {
        if offset + 12 > data.len() {
            return Err(format!(
                "数据不足，无法读取 TableDescriptor at 0x{:X}",
                offset
            ));
        }
        Ok(TableDescriptor {
            offset: u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()),
            num_entries: u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()),
            entry_size: u32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap()),
        })
    }

    // ========================================================================
    // 解析 Partition 表
    // ========================================================================

    fn parse_partitions(
        data: &[u8],
        offset: usize,
        count: u32,
        entry_size: usize,
    ) -> Result<Vec<LpMetadataPartition>, String> {
        if entry_size < 48 {
            return Err(format!(
                "Partition entry_size {} 过小（至少 48）",
                entry_size
            ));
        }

        let mut partitions = Vec::with_capacity(count as usize);
        let name_len = 36usize.min(entry_size.saturating_sub(16));

        for i in 0..count {
            let eo = offset + i as usize * entry_size;
            if eo + entry_size > data.len() {
                warn!("Partition[{}] 超出数据范围", i);
                break;
            }

            let name = String::from_utf8_lossy(&data[eo..eo + name_len])
                .trim_end_matches('\0')
                .to_string();

            // AOSP 标准偏移：name[36] + attributes(u32@36) + first_extent(u32@40) + num_extents(u32@44) + group(u32@48)
            let attributes = u32::from_le_bytes(data[eo + 36..eo + 40].try_into().unwrap());
            let first_extent_index = u32::from_le_bytes(data[eo + 40..eo + 44].try_into().unwrap());
            let num_extents = u32::from_le_bytes(data[eo + 44..eo + 48].try_into().unwrap());
            let group_index = if entry_size >= 52 {
                u32::from_le_bytes(data[eo + 48..eo + 52].try_into().unwrap())
            } else {
                0
            };

            trace!(
                "  partition[{}]: name={}, attr=0x{:X}, first_ext={}, num_ext={}, group={}",
                i, name, attributes, first_extent_index, num_extents, group_index
            );

            partitions.push(LpMetadataPartition {
                name,
                attributes,
                first_extent_index,
                num_extents,
                group_index,
            });
        }

        Ok(partitions)
    }

    // ========================================================================
    // 解析 Extent 表
    // ========================================================================

    fn parse_extents(
        data: &[u8],
        offset: usize,
        count: u32,
        entry_size: usize,
    ) -> Result<Vec<LpMetadataExtent>, String> {
        if entry_size < 24 {
            return Err(format!("Extent entry_size {} 过小（至少 24）", entry_size));
        }

        let mut extents = Vec::with_capacity(count as usize);

        for i in 0..count {
            let eo = offset + i as usize * entry_size;
            if eo + entry_size > data.len() {
                warn!("Extent[{}] 超出数据范围", i);
                break;
            }

            // AOSP 标准 24 字节布局：
            //   0..8:  num_sectors(u64)
            //   8..12: target_type(u32)  0=LINEAR, 1=ZERO
            //   12..20: target_data(u64)  LINEAR 时为物理扇区偏移
            //   20..24: target_source(u32)
            let num_sectors = u64::from_le_bytes(data[eo..eo + 8].try_into().unwrap());
            let target_type = u32::from_le_bytes(data[eo + 8..eo + 12].try_into().unwrap());
            let target_data = u64::from_le_bytes(data[eo + 12..eo + 20].try_into().unwrap());
            let target_source = u32::from_le_bytes(data[eo + 20..eo + 24].try_into().unwrap());

            if num_sectors > 0 || target_data > 0 {
                trace!(
                    "  extent[{}]: sectors={}, type={}, data=0x{:X}, source={}",
                    i, num_sectors, target_type, target_data, target_source
                );
            }

            extents.push(LpMetadataExtent {
                num_sectors,
                target_type,
                target_data,
                target_source,
            });
        }

        Ok(extents)
    }

    // ========================================================================
    // 解析 Group 表
    // ========================================================================

    fn parse_groups(
        data: &[u8],
        offset: usize,
        count: u32,
        entry_size: usize,
    ) -> Vec<LpMetadataGroup> {
        let mut groups = Vec::with_capacity(count as usize);
        let name_len = 36usize.min(entry_size.saturating_sub(12));

        for i in 0..count {
            let eo = offset + i as usize * entry_size;
            if eo + entry_size > data.len() {
                break;
            }

            let name = String::from_utf8_lossy(&data[eo..eo + name_len])
                .trim_end_matches('\0')
                .to_string();

            // AOSP 标准：name[36] + flags(u32@36) + maximum_size(u64@40)
            let flags = u32::from_le_bytes(data[eo + 36..eo + 40].try_into().unwrap());
            let maximum_size = u64::from_le_bytes(data[eo + 40..eo + 48].try_into().unwrap());

            trace!(
                "  group[{}]: name={}, flags={}, max_size={}",
                i, name, flags, maximum_size
            );

            groups.push(LpMetadataGroup {
                name,
                maximum_size,
                flags,
            });
        }

        groups
    }

    // ========================================================================
    // 查找分区
    // ========================================================================

    /// 按名称查找 logical partition，返回 (起始扇区偏移 × block_size, 总字节数)
    pub fn find_partition(&self, name: &str) -> Option<(u64, u64)> {
        for part in &self.partitions {
            if part.name == name {
                let mut total_sectors = 0u64;
                let mut start_sector = 0u64;
                let mut first = true;

                for i in 0..part.num_extents {
                    let ext_idx = part.first_extent_index + i;
                    if let Some(ext) = self.extents.get(ext_idx as usize) {
                        if first {
                            start_sector = ext.target_data;
                            first = false;
                        }
                        total_sectors += ext.num_sectors;
                    }
                }

                let sector_size = self.block_size as u64;
                return Some((start_sector * sector_size, total_sectors * sector_size));
            }
        }
        None
    }

    /// 列出所有 logical partition
    pub fn list_partitions(&self) -> Vec<(String, u64, u64)> {
        self.partitions
            .iter()
            .map(|p| {
                let (off, size) = self.find_partition(&p.name).unwrap_or((0, 0));
                (p.name.clone(), off, size)
            })
            .collect()
    }

    /// 智能查找分区，自动处理 A/B 槽位
    pub fn find_partition_smart(&self, name: &str) -> Option<(String, u64, u64)> {
        // 1. 精确匹配
        if let Some((off, size)) = self.find_partition(name) {
            return Some((name.to_string(), off, size));
        }

        // 2. 尝试 _a / _b 后缀
        for suffix in &["_a", "_b"] {
            let slot_name = format!("{}{}", name, suffix);
            if let Some((off, size)) = self.find_partition(&slot_name) {
                return Some((slot_name, off, size));
            }
        }

        None
    }
}
