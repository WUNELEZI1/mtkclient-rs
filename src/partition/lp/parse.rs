//! liblp super partition metadata parsing logic

use log::{debug, trace, warn};

use super::LpMetadataGroup;
use super::types::{
    LpMetadataExtent, LpMetadataGeometry, LpMetadataHeader, LpMetadataPartition, TableDescriptor,
};
use super::{GEOMETRY_MAGIC, HEADER_MAGIC};

/// Parsed super partition metadata
#[derive(Debug, Clone)]
pub struct SuperMetadata {
    pub partitions: Vec<LpMetadataPartition>,
    pub extents: Vec<LpMetadataExtent>,
    /// Logical block size (sector size, 512 bytes by default)
    pub block_size: u32,
}

// ============================================================================
// SuperMetadata main parsing logic
// ============================================================================

impl SuperMetadata {
    /// Parse metadata from raw super/metadata partition data
    ///
    /// Strategy:
    /// 1. Scan for "0PLA" (LP_METADATA_HEADER_MAGIC = 0x414C5030)
    /// 2. Walk backwards to find Geometry (magic = 0x616C4467)
    /// 3. If Geometry is not found, parse directly from the Header offset
    /// 4. Locate each table via TableDescriptor and parse partition/extent/group
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        let max_scan = std::cmp::min(data.len(), 0x80000); // scan the first 512 KB

        // Step 1: scan for the Header magic "0PLA"
        let header_offset = Self::find_header_magic(data, max_scan)?;
        debug!(
            "找到 LP_METADATA_HEADER_MAGIC at offset 0x{:04X}",
            header_offset
        );

        // Step 2: try to find Geometry
        let geometry = Self::find_geometry(data, header_offset, max_scan);

        // Step 3: parse the Header (including TableDescriptor)
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

        // Step 4: parse each table via TableDescriptor
        // Table position = header_offset + header_size + descriptor.offset
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

        // Determine block_size: prefer geometry, otherwise default to 512
        let block_size = geometry
            .as_ref()
            .map(|g| g.logical_block_size)
            .unwrap_or(512);

        Ok(SuperMetadata {
            partitions,
            extents,
            block_size,
        })
    }

    // ========================================================================
    // Magic scanning
    // ========================================================================

    /// Scan for the "0PLA" Header magic
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
        // Also try a byte-by-byte scan (some offsets may not be 512-byte aligned)
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

    /// Walk backwards from header_offset to find Geometry
    fn find_geometry(
        data: &[u8],
        header_offset: usize,
        _max_scan: usize,
    ) -> Option<LpMetadataGeometry> {
        // Geometry is usually at a multiple of 0x1000 before the header
        // Scan the region before header_offset
        let scan_start = header_offset.saturating_sub(0x40000);
        for offset in (scan_start..header_offset).rev().step_by(512) {
            if offset + 52 > data.len() {
                continue;
            }
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic == GEOMETRY_MAGIC {
                debug!("找到 LP_METADATA_GEOMETRY_MAGIC at offset 0x{:04X}", offset);
                // AOSP fixes the sector size at 512 (LP_SECTOR_SIZE); some vendors store
                // logical_block_size at geometry offset 48 (4096 is common). In the AOSP spec
                // that offset is actually the low half of first_logical_sector (uint64), so reading it
                // directly as block_size may yield garbage. Only trust a valid power of two and fall back to 512 otherwise, to avoid miscalculating dynamic partition sizes.
                let raw = if offset + 52 <= data.len() {
                    u32::from_le_bytes(data[offset + 48..offset + 52].try_into().unwrap())
                } else {
                    0
                };
                let logical_block_size = if raw.is_power_of_two() && (512..=65536).contains(&raw) {
                    raw
                } else {
                    512
                };
                return Some(LpMetadataGeometry { logical_block_size });
            }
        }
        warn!("未找到 LP_METADATA_GEOMETRY_MAGIC，使用默认值");
        None
    }

    // ========================================================================
    // Parse Header
    // ========================================================================

    fn parse_header(data: &[u8], offset: usize) -> Result<LpMetadataHeader, String> {
        // The Header needs at least 128 bytes to read the 4 TableDescriptors
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

        // tables_size is at offset 44 (after the 32-byte SHA256 header_checksum)
        let tables_size = u32::from_le_bytes(data[offset + 44..offset + 48].try_into().unwrap());

        // The 4 TableDescriptors start at offset 80, 12 bytes each
        let partitions = Self::read_table_descriptor(data, offset + 80)?;
        let extents = Self::read_table_descriptor(data, offset + 92)?;
        let groups = Self::read_table_descriptor(data, offset + 104)?;
        let block_devices = Self::read_table_descriptor(data, offset + 116)?;

        Ok(LpMetadataHeader {
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
    // Parse the Partition table
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

            // AOSP standard offsets: name`36` + attributes(u32@36) + first_extent(u32@40) + num_extents(u32@44) + group(u32@48)
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
            });
        }

        Ok(partitions)
    }

    // ========================================================================
    // Parse the Extent table
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

            // AOSP standard 24-byte layout:
            //   0..8:  num_sectors(u64)
            //   8..12: target_type(u32)  0=LINEAR, 1=ZERO
            //   12..20: target_data(u64)  physical sector offset when LINEAR
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
                target_data,
            });
        }

        Ok(extents)
    }

    // ========================================================================
    // Parse the Group table
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

            // AOSP standard: name`36` + flags(u32@36) + maximum_size(u64@40)
            let flags = u32::from_le_bytes(data[eo + 36..eo + 40].try_into().unwrap());
            let maximum_size = u64::from_le_bytes(data[eo + 40..eo + 48].try_into().unwrap());

            trace!(
                "  group[{}]: name={}, flags={}, max_size={}",
                i, name, flags, maximum_size
            );

            groups.push(LpMetadataGroup {});
        }

        groups
    }

    // ========================================================================
    // Find a partition
    // ========================================================================

    /// Find a logical partition by name; returns (start sector offset × block_size, total byte count)
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

    /// Smart lookup that automatically handles A/B slots
    pub fn find_partition_smart(&self, name: &str) -> Option<(String, u64, u64)> {
        // 1. Exact match
        if let Some((off, size)) = self.find_partition(name) {
            return Some((name.to_string(), off, size));
        }

        // 2. Try _a / _b suffixes
        for suffix in &["_a", "_b"] {
            let slot_name = format!("{}{}", name, suffix);
            if let Some((off, size)) = self.find_partition(&slot_name) {
                return Some((slot_name, off, size));
            }
        }

        None
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[path = "parse/tests.rs"]
mod tests;
