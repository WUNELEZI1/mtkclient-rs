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

pub(crate) mod parse;
pub(crate) mod types;

// ============================================================================
// AOSP 标准 Magic 值
// ============================================================================

/// LP_METADATA_GEOMETRY_MAGIC = 0x616C4467 ("gDla" in LE)
/// 注：AOSP metadata_format.h 定义为小端 0x616C4467。此前误写为 0x674C4164
/// （那是 "gDla" 的大端表示），而扫描用 from_le_bytes 小端读取，导致永远匹配不上。
const GEOMETRY_MAGIC: u32 = 0x616C4467;
/// LP_METADATA_HEADER_MAGIC = 0x414C5030 ("0PLA" in LE)
const HEADER_MAGIC: u32 = 0x414C5030;

/// LpMetadataPartitionGroup (变长，默认 entry_size = 0x30 = 48 字节)
///
/// AOSP 布局：
///   offset 0:  name(char[36])
///   offset 36: flags(u32)
///   offset 40: maximum_size(u64)
#[derive(Debug, Clone)]
pub struct LpMetadataGroup {}

// 重新导出子模块中的 pub 项，确保通过 crate::partition::lp::Item 仍可访问
pub use parse::SuperMetadata;
