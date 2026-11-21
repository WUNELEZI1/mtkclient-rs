//! Android Dynamic Partition (liblp) parser.
//!
//! Follows AOSP system/core/fs_mgr/liblp/include/liblp/metadata_format.h exactly.
//!
//! Data layout (super partition or metadata partition):
//!   0x0000  reserved (LP_PARTITION_RESERVED_BYTES = 4096)
//!   0x1000  primary Geometry   (LpMetadataGeometry, fixed 4096 bytes)
//!   0x2000  backup Geometry    (LpMetadataGeometry, fixed 4096 bytes)
//!   0x3000  primary Metadata   (Header + Tables)
//!   ...     backup Metadata(s)
//!   ...     logical partition data
//!
//! Inside Metadata:
//!   Header (fixed 256 bytes, holding 4 LpMetadataTableDescriptor entries)
//!   Partition table
//!   Extent table
//!   Group table
//!   Block device table
//!
//! Note: MTK Virtual A/B devices (e.g. MT6768) may store metadata in a standalone
//! metadata partition (32MB) instead of the super partition header; the super
//! partition may then be entirely zero.

pub(crate) mod parse;
pub(crate) mod types;

// ============================================================================
// AOSP standard magic values
// ============================================================================

/// LP_METADATA_GEOMETRY_MAGIC = 0x616C4467 ("gDla" in LE)
/// Note: AOSP metadata_format.h defines this as little-endian 0x616C4467. It was
/// previously miswritten as 0x674C4164 (the big-endian form of "gDla"), while the
/// scan compares with from_le_bytes, so it never matched.
const GEOMETRY_MAGIC: u32 = 0x616C4467;
/// LP_METADATA_HEADER_MAGIC = 0x414C5030 ("0PLA" in LE)
const HEADER_MAGIC: u32 = 0x414C5030;

/// LpMetadataPartitionGroup (variable length, default entry_size = 0x30 = 48 bytes)
///
/// AOSP layout:
///   offset 0:  name(char`36`)
///   offset 36: flags(u32)
///   offset 40: maximum_size(u64)
#[derive(Debug, Clone)]
pub struct LpMetadataGroup {}

// Re-export pub items from submodules so they stay reachable via crate::partition::lp::Item
pub use parse::SuperMetadata;
