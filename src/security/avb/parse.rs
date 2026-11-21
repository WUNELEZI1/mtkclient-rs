//! Descriptor types and descriptor-content parsing for vbmeta images.

/// Descriptor tags (libavb avb_descriptor.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorTag {
    Property,
    Hashtree,
    Hash,
    KernelCmdline,
    ChainPartition,
    Unknown(u64),
}

impl From<u64> for DescriptorTag {
    fn from(v: u64) -> Self {
        match v {
            1 => Self::Property,
            2 => Self::Hashtree,
            3 => Self::Hash,
            4 => Self::KernelCmdline,
            5 => Self::ChainPartition,
            other => Self::Unknown(other),
        }
    }
}

impl Descriptor {
    /// Readable name of the descriptor type (for online logs).
    pub fn tag_debug_name(&self) -> &'static str {
        match self.tag {
            DescriptorTag::Property => "Property",
            DescriptorTag::Hashtree => "Hashtree",
            DescriptorTag::Hash => "Hash",
            DescriptorTag::KernelCmdline => "KernelCmdline",
            DescriptorTag::ChainPartition => "ChainPartition",
            DescriptorTag::Unknown(_) => "Unknown",
        }
    }
}

impl DescriptorTag {
    pub(crate) fn name(self) -> String {
        match self {
            Self::Property => "Property".into(),
            Self::Hashtree => "Hashtree".into(),
            Self::Hash => "Hash".into(),
            Self::KernelCmdline => "Kernel Cmdline".into(),
            Self::ChainPartition => "Chain Partition".into(),
            Self::Unknown(v) => format!("Unknown ({v})"),
        }
    }
}

/// Parsed descriptor (summary info).
#[derive(Debug, Clone)]
pub struct Descriptor {
    pub tag: DescriptorTag,
    /// Convenience field: partition name associated with Property/ChainPartition/KernelCmdline, if any.
    pub partition_name: Option<String>,
}

/// Parse the partition name inside a descriptor (using real libavb field offsets per descriptor).
///
/// Layout (relative to the descriptor start; tag(8)+length(8) prefix is 16 bytes):
/// - Hash: image_size(8) + `hash_algorithm[32]` + name_len(4)@56 + salt_len(4) +
///   digest_len(4) + flags(4); name at @72;
/// - Hashtree v1.1+: dm_verity(8) + image_size(8) + tree_offset(8) + tree_size(8) +
///   hash_algorithm(4) + name_len(4)@52 + salt_len(4) + root_digest_len(4) + flags(4) +
///   reserved`60`; name at @128. v1.0 has no reserved; name at @68;
/// - Chain: location(4) + name_len(4)@20 + key_len(4) + flags(4); name at @32.
pub(crate) fn parse_partition_name(tag: &DescriptorTag, raw: &[u8]) -> Option<String> {
    /// Read the length at `name_len_off` and the name at `name_off`, validating it is printable.
    fn name_at(raw: &[u8], name_len_off: usize, name_off: usize) -> Option<String> {
        if name_len_off + 4 > raw.len() {
            return None;
        }
        let nlen =
            u32::from_be_bytes(raw[name_len_off..name_len_off + 4].try_into().ok()?) as usize;
        if nlen == 0 || nlen > 64 || name_off + nlen > raw.len() {
            return None;
        }
        let name = &raw[name_off..name_off + nlen];
        if name.iter().all(|c| c.is_ascii_graphic()) {
            Some(String::from_utf8_lossy(name).into_owned())
        } else {
            None
        }
    }

    match tag {
        DescriptorTag::Hash => name_at(raw, 16 + 8 + 32, 16 + 8 + 32 + 16),
        DescriptorTag::Hashtree => name_at(raw, 16 + 32 + 4, 16 + 32 + 16 + 60)
            .or_else(|| name_at(raw, 16 + 32 + 4, 16 + 32 + 16)),
        DescriptorTag::ChainPartition => name_at(raw, 16 + 4, 16 + 16),
        _ => None,
    }
}
