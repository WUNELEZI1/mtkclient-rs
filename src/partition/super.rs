//! Android Dynamic Partition (liblp) 解析器
//!
//! 支持从 super 分区镜像中解析 logical partition（system、vendor、product 等）。
//! 参考 AOSP system/core/fs_mgr/liblp。

use log::{debug, trace};

/// LP_METADATA_GEOMETRY magic = "ALAG" = 0x414C4147
const LP_METADATA_GEOMETRY_MAGIC: u32 = 0x414C4147;
/// LP_METADATA_HEADER magic = 0x504C0A0B
const LP_METADATA_HEADER_MAGIC: u32 = 0x504C0A0B;

/// Geometry 结构（位于 super 分区头部，偏移 0 或 0x1000 倍数）
#[derive(Debug, Clone)]
pub struct LpMetadataGeometry {
    pub magic: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub header_size: u32,
    pub header_checksum: u32,
}

/// Metadata Header 结构
#[derive(Debug, Clone)]
pub struct LpMetadataHeader {
    pub magic: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub header_size: u32,
    pub header_checksum: u32,
    pub tables_size: u64,
    pub tables_checksum: u64,
    pub partitions_count: u32,
    pub extents_count: u32,
    pub groups_count: u32,
    pub block_devices_count: u32,
}

/// Logical Partition 条目
#[derive(Debug, Clone)]
pub struct LpMetadataPartition {
    pub name: String,
    pub attributes: u32,
    pub first_extent_index: u32,
    pub num_extents: u32,
    pub group_index: u64,
}

/// Extent 条目（描述数据在物理设备上的位置）
#[derive(Debug, Clone)]
pub struct LpMetadataExtent {
    pub num_sectors: u64,
    pub target_type: u32,
    pub target_source: u32,
    pub target_data: u64,
}

/// 解析后的 Super 分区元数据
#[derive(Debug, Clone)]
pub struct SuperMetadata {
    pub geometry: LpMetadataGeometry,
    pub header: LpMetadataHeader,
    pub partitions: Vec<LpMetadataPartition>,
    pub extents: Vec<LpMetadataExtent>,
    pub block_size: u32, // 通常为 4096
}

impl SuperMetadata {
    /// 从 super 分区原始数据中解析元数据
    ///
    /// 扫描 0x1000 倍数偏移查找 LP_METADATA_GEOMETRY_MAGIC，
    /// 然后解析 header → partition table → extent table。
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        // 扫描 geometry（通常在前 256KB 内）
        let geometry = Self::find_geometry(data)?;
        trace!("Super geometry found: {:?}", geometry);

        // geometry 之后是 header（偏移通常是 geometry.header_size 之后）
        // 实际上 geometry 只占前 4096 字节，header 在 4096 偏移处
        let header_offset = 4096usize;
        let header = Self::parse_header(data, header_offset)?;
        trace!("Super header: {:?}", header);

        // partition table 紧随 header 之后
        let partitions_offset = header_offset + header.header_size as usize;
        let partitions = Self::parse_partitions(data, partitions_offset, header.partitions_count)?;
        debug!(
            "Super partitions: {} entries",
            partitions.len()
        );

        // extent table 紧随 partition table 之后
        let _extent_entry_size = 32usize; // sizeof(LpMetadataExtent)
        let extents_offset = partitions_offset
            + header.partitions_count as usize * 256; // partition entry size = 256
        let extents = Self::parse_extents(data, extents_offset, header.extents_count)?;

        // block size 默认 4096
        let block_size = 4096u32;

        Ok(SuperMetadata {
            geometry,
            header,
            partitions,
            extents,
            block_size,
        })
    }

    /// 按名称查找 logical partition，返回 (起始字节偏移, 大小字节)
    pub fn find_partition(&self, name: &str) -> Option<(u64, u64)> {
        for part in &self.partitions {
            if part.name == name {
                // 计算该分区的总大小和起始位置
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

                let block_size = self.block_size as u64;
                return Some((start_sector * block_size, total_sectors * block_size));
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

    fn find_geometry(data: &[u8]) -> Result<LpMetadataGeometry, String> {
        // 扫描 0x1000 倍数偏移
        let max_scan = std::cmp::min(data.len(), 0x40000); // 扫描前 256KB
        for offset in (0..max_scan).step_by(0x1000) {
            if offset + 16 > data.len() {
                break;
            }
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic == LP_METADATA_GEOMETRY_MAGIC {
                return Ok(LpMetadataGeometry {
                    magic,
                    major_version: u16::from_le_bytes(data[offset + 4..offset + 6].try_into().unwrap()),
                    minor_version: u16::from_le_bytes(data[offset + 6..offset + 8].try_into().unwrap()),
                    header_size: u32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap()),
                    header_checksum: u32::from_le_bytes(data[offset + 12..offset + 16].try_into().unwrap()),
                });
            }
        }
        Err("未找到 LP_METADATA_GEOMETRY (magic=0x414C4147)，可能不是动态分区".to_string())
    }

    fn parse_header(data: &[u8], offset: usize) -> Result<LpMetadataHeader, String> {
        if offset + 28 > data.len() {
            return Err("数据不足，无法解析 header".to_string());
        }
        let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        if magic != LP_METADATA_HEADER_MAGIC {
            return Err(format!(
                "Header magic 不匹配: 期望 0x{:08X}, 实际 0x{:08X}",
                LP_METADATA_HEADER_MAGIC, magic
            ));
        }

        // AOSP 结构：
        // uint32_t magic;
        // uint16_t major_version;
        // uint16_t minor_version;
        // uint32_t header_size;
        // uint32_t header_checksum;
        // uint64_t tables_size;
        // uint64_t tables_checksum;
        // uint32_t partitions_count;
        // uint32_t extents_count;
        // uint32_t groups_count;
        // uint32_t block_devices_count;
        let tables_size = u64::from_le_bytes(data[offset + 16..offset + 24].try_into().unwrap());
        let tables_checksum = u64::from_le_bytes(data[offset + 24..offset + 32].try_into().unwrap());
        let partitions_count = u32::from_le_bytes(data[offset + 32..offset + 36].try_into().unwrap());
        let extents_count = u32::from_le_bytes(data[offset + 36..offset + 40].try_into().unwrap());
        let groups_count = u32::from_le_bytes(data[offset + 40..offset + 44].try_into().unwrap());
        let block_devices_count = u32::from_le_bytes(data[offset + 44..offset + 48].try_into().unwrap());

        Ok(LpMetadataHeader {
            magic,
            major_version: u16::from_le_bytes(data[offset + 4..offset + 6].try_into().unwrap()),
            minor_version: u16::from_le_bytes(data[offset + 6..offset + 8].try_into().unwrap()),
            header_size: u32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap()),
            header_checksum: u32::from_le_bytes(data[offset + 12..offset + 16].try_into().unwrap()),
            tables_size,
            tables_checksum,
            partitions_count,
            extents_count,
            groups_count,
            block_devices_count,
        })
    }

    fn parse_partitions(
        data: &[u8],
        offset: usize,
        count: u32,
    ) -> Result<Vec<LpMetadataPartition>, String> {
        let mut partitions = Vec::with_capacity(count as usize);
        let entry_size = 256usize; // AOSP LpMetadataPartition 大小

        for i in 0..count {
            let entry_offset = offset + i as usize * entry_size;
            if entry_offset + entry_size > data.len() {
                break;
            }
            let name_bytes = &data[entry_offset..entry_offset + 36];
            let name = String::from_utf8_lossy(name_bytes)
                .trim_end_matches('\0')
                .to_string();
            let attributes = u32::from_le_bytes(
                data[entry_offset + 36..entry_offset + 40].try_into().unwrap(),
            );
            let first_extent_index = u32::from_le_bytes(
                data[entry_offset + 40..entry_offset + 44].try_into().unwrap(),
            );
            let num_extents = u32::from_le_bytes(
                data[entry_offset + 44..entry_offset + 48].try_into().unwrap(),
            );
            let group_index = u64::from_le_bytes(
                data[entry_offset + 48..entry_offset + 56].try_into().unwrap(),
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

    fn parse_extents(
        data: &[u8],
        offset: usize,
        count: u32,
    ) -> Result<Vec<LpMetadataExtent>, String> {
        let mut extents = Vec::with_capacity(count as usize);
        let entry_size = 32usize; // AOSP LpMetadataExtent 大小

        for i in 0..count {
            let entry_offset = offset + i as usize * entry_size;
            if entry_offset + entry_size > data.len() {
                break;
            }
            let num_sectors = u64::from_le_bytes(
                data[entry_offset..entry_offset + 8].try_into().unwrap(),
            );
            let target_type = u32::from_le_bytes(
                data[entry_offset + 8..entry_offset + 12].try_into().unwrap(),
            );
            let target_source = u32::from_le_bytes(
                data[entry_offset + 12..entry_offset + 16].try_into().unwrap(),
            );
            let target_data = u64::from_le_bytes(
                data[entry_offset + 16..entry_offset + 24].try_into().unwrap(),
            );

            extents.push(LpMetadataExtent {
                num_sectors,
                target_type,
                target_source,
                target_data,
            });
        }

        Ok(extents)
    }
}
