//! liblp data structure definitions
//!
//! All AOSP liblp metadata format structs (packed, no padding).

/// LpMetadataGeometry (4096 bytes, aligned to LP_SECTOR_SIZE)
///
/// AOSP layout:
///   offset 0:  magic(u32) = 0x616C4467
///   offset 4:  struct_size(u32)
///   offset 8:  checksum(SHA256, 32 bytes)
///   offset 40: metadata_max_size(u32)
///   offset 44: metadata_slot_count(u32)
///   offset 48: logical_block_size(u32)
#[derive(Debug, Clone)]
pub struct LpMetadataGeometry {
    pub logical_block_size: u32,
}

/// LpMetadataTableDescriptor (12 bytes)
///
///   offset 0: offset(u32)   — table position, relative to the end of the header
///   offset 4: num_entries(u32) — number of entries
///   offset 8: entry_size(u32)  — bytes per entry
#[derive(Debug, Clone)]
pub(crate) struct TableDescriptor {
    pub offset: u32,
    pub num_entries: u32,
    pub entry_size: u32,
}

/// LpMetadataHeader (256 bytes)
///
/// AOSP layout:
///   offset 0:  magic(u32) = 0x414C5030 ("0PLA")
///   offset 4:  major_version(u16)
///   offset 6:  minor_version(u16)
///   offset 8:  header_size(u32) = 256
///   offset 12: header_checksum(SHA256, 32 bytes)
///   offset 44: tables_size(u32)
///   offset 48: tables_checksum(SHA256, 32 bytes)
///   offset 80: partitions(TableDescriptor, 12 bytes)
///   offset 92: extents(TableDescriptor, 12 bytes)
///   offset 104: groups(TableDescriptor, 12 bytes)
///   offset 116: block_devices(TableDescriptor, 12 bytes)
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

/// LpMetadataPartition (variable length, default entry_size = 0x34 = 52 bytes)
///
/// AOSP layout:
///   offset 0:  name(char`36`)
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

/// This partition name gets the current slot suffix appended (e.g. system → system_a)
pub const LP_PARTITION_ATTR_SLOT_SUFFIXED: u32 = 0x00000002;

impl LpMetadataPartition {
    /// Whether it carries a slot suffix (A/B partition)
    pub fn is_slot_suffixed(&self) -> bool {
        (self.attributes & LP_PARTITION_ATTR_SLOT_SUFFIXED) != 0
    }
}

/// LpMetadataExtent (variable length, default entry_size = 0x18 = 24 bytes)
///
/// AOSP layout:
///   offset 0:  num_sectors(u64)
///   offset 8:  target_type(u32)  — 0=LINEAR, 1=ZERO
///   offset 12: target_data(u64)   — LINEAR: physical sector offset
///   offset 20: target_source(u32) — LINEAR: block_devices table index
#[derive(Debug, Clone)]
pub struct LpMetadataExtent {
    pub num_sectors: u64,
    pub target_data: u64,
}
