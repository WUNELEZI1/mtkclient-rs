//! In-house avbtool: parse and rewrite Android Verified Boot vbmeta images
//!
//! Covers the core `avbtool` (libavb) capabilities in pure Rust with no dependencies:
//! - [`VbmetaImage::parse`]: fully parse the vbmeta header (0xB0 bytes) and all descriptors;
//! - [`VbmetaImage::info`]: output equivalent to `avbtool info-image`;
//! - [`VbmetaImage::disable_verification`]: rewrite flags to turn off verity/verification
//!   (equivalent to `avbtool make_vbmeta_image --flags`; the bootloader reads flags and skips
//!   verification, avoiding a dm-verity boot failure);
//! - `VbmetaImage::strip_descriptors`: optionally clears all hash/hashtree descriptors
//!   (equivalent to repacking an empty vbmeta; the most aggressive option).
//!
//! All multi-byte fields are **big-endian** as required by libavb.

use std::fmt::Write as _;

/// vbmeta magic.
pub const AVB_MAGIC: &[u8; 4] = b"AVB0";
/// vbmeta header size (0x80 header + 48-byte release_string).
pub const AVB_HEADER_SIZE: usize = 0xB0;

/// Flag bit: hashtree disabled (dm-verity off).
pub const FLAG_HASHTREE_DISABLED: u32 = 0x01;
/// Flag bit: signature verification disabled.
pub const FLAG_VERIFICATION_DISABLED: u32 = 0x02;

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
    fn name(self) -> String {
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

/// vbmeta header fields (all converted from big-endian).
#[derive(Debug, Clone)]
pub struct VbmetaHeader {
    pub required_libavb_version_major: u32,
    pub required_libavb_version_minor: u32,
    pub authentication_data_block_size: u64,
    pub auxiliary_data_block_size: u64,
    pub algorithm_type: u32,
    pub hash_offset: u64,
    pub hash_size: u64,
    pub signature_offset: u64,
    pub signature_size: u64,
    pub public_key_offset: u64,
    pub public_key_size: u64,
    pub public_key_metadata_offset: u64,
    pub public_key_metadata_size: u64,
    pub descriptors_offset: u64,
    pub descriptors_size: u64,
    pub rollback_index: u64,
    pub flags: u32,
    pub rollback_index_location: u32,
    pub release_string: String,
}

/// Full vbmeta image (header + descriptor list + raw auth/aux blocks).
#[derive(Debug, Clone)]
pub struct VbmetaImage {
    pub header: VbmetaHeader,
    pub descriptors: Vec<Descriptor>,
    /// Raw bytes of the authentication block (signature + hash).
    pub auth_block: Vec<u8>,
    /// Raw bytes of the auxiliary block (public key + descriptors + public key metadata).
    pub aux_block: Vec<u8>,
}

#[derive(Debug)]
pub enum AvbError {
    TooSmall,
    BadMagic,
    BadDescriptor,
}

impl std::fmt::Display for AvbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSmall => write!(f, "镜像太小，不足 vbmeta 头（0xB0 字节）"),
            Self::BadMagic => write!(f, "非 vbmeta 格式（缺少 AVB0 魔数）"),
            Self::BadDescriptor => write!(f, "descriptor 头不完整或长度越界"),
        }
    }
}

impl std::error::Error for AvbError {}

fn rd_u64(b: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(b[off..off + 8].try_into().unwrap())
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

impl VbmetaImage {
    /// Parse a vbmeta image (full data: header + auth block + aux block).
    pub fn parse(data: &[u8]) -> Result<Self, AvbError> {
        if data.len() < AVB_HEADER_SIZE {
            return Err(AvbError::TooSmall);
        }
        if &data[0..4] != AVB_MAGIC {
            return Err(AvbError::BadMagic);
        }

        let header = VbmetaHeader {
            required_libavb_version_major: rd_u32(data, 0x04),
            required_libavb_version_minor: rd_u32(data, 0x08),
            authentication_data_block_size: rd_u64(data, 0x0C),
            auxiliary_data_block_size: rd_u64(data, 0x14),
            algorithm_type: rd_u32(data, 0x1C),
            hash_offset: rd_u64(data, 0x20),
            hash_size: rd_u64(data, 0x28),
            signature_offset: rd_u64(data, 0x30),
            signature_size: rd_u64(data, 0x38),
            public_key_offset: rd_u64(data, 0x40),
            public_key_size: rd_u64(data, 0x48),
            public_key_metadata_offset: rd_u64(data, 0x50),
            public_key_metadata_size: rd_u64(data, 0x58),
            descriptors_offset: rd_u64(data, 0x60),
            descriptors_size: rd_u64(data, 0x68),
            rollback_index: rd_u64(data, 0x70),
            flags: rd_u32(data, 0x78),
            rollback_index_location: rd_u32(data, 0x7C),
            release_string: String::from_utf8_lossy(&data[0x80..0xB0])
                .trim_end_matches('\0')
                .to_string(),
        };

        // The auth block follows the header, and the aux block follows the auth block.
        let auth_end = AVB_HEADER_SIZE + header.authentication_data_block_size as usize;
        let aux_end = auth_end + header.auxiliary_data_block_size as usize;
        if auth_end > data.len() || aux_end > data.len() {
            return Err(AvbError::BadDescriptor);
        }
        let auth_block = data[AVB_HEADER_SIZE..auth_end].to_vec();
        let aux_block = data[auth_end..aux_end].to_vec();

        // Descriptors live in the aux block (descriptors_offset is relative to the aux block start).
        let mut descriptors = Vec::new();
        let d_start = header.descriptors_offset as usize;
        let d_end = d_start + header.descriptors_size as usize;
        if d_end > aux_block.len() {
            return Err(AvbError::BadDescriptor);
        }
        let mut p = d_start;
        while p + 16 <= d_end {
            let tag = DescriptorTag::from(rd_u64(&aux_block, p));
            let len = rd_u64(&aux_block, p + 8) as usize;
            if p + 16 + len > d_end {
                return Err(AvbError::BadDescriptor);
            }
            let raw = aux_block[p..p + 16 + len].to_vec();
            descriptors.push(Descriptor {
                tag,
                partition_name: parse_partition_name(&tag, &raw),
            });
            p += 16 + len;
        }

        Ok(Self {
            header,
            descriptors,
            auth_block,
            aux_block,
        })
    }

    /// Format equivalent to `avbtool info-image`.
    pub fn info(&self) -> String {
        let h = &self.header;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "Minimum libavb version: {}.{}",
            h.required_libavb_version_major, h.required_libavb_version_minor
        );
        let _ = writeln!(s, "Header Size: {AVB_HEADER_SIZE} bytes");
        let _ = writeln!(s, "Release String: '{}'", h.release_string);
        let _ = writeln!(s, "Descriptors:");
        for d in &self.descriptors {
            match d.partition_name.as_deref() {
                Some(name) => {
                    let _ = writeln!(s, "  {}: {}", d.tag.name(), name);
                }
                None => {
                    let _ = writeln!(s, "  {}:", d.tag.name());
                }
            }
        }
        let _ = writeln!(s, "Algorithm Type: {}", h.algorithm_type);
        let _ = writeln!(s, "Rollback Index: {}", h.rollback_index);
        let _ = writeln!(s, "Rollback Index Location: {}", h.rollback_index_location);
        let _ = writeln!(s, "Flags: {}", h.flags);
        let _ = writeln!(
            s,
            "Hashtree disabled: {}",
            h.flags & FLAG_HASHTREE_DISABLED != 0
        );
        let _ = writeln!(
            s,
            "Verification disabled: {}",
            h.flags & FLAG_VERIFICATION_DISABLED != 0
        );
        s
    }

    /// Rewrite flags and rebuild the image. `flags` is usually passed as
    /// `FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED` (=3, recommended for flashing).
    ///
    /// Note: changing flags invalidates the original signature, but the bootloader reads the flags
    /// first, so with verification off it will not refuse to boot (same as official avbtool).
    pub fn disable_verification(&self, flags: u32) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(AVB_HEADER_SIZE + self.auth_block.len() + self.aux_block.len());
        out.extend_from_slice(&encode_header(&self.rebuild_header(flags)));
        out.extend_from_slice(&self.auth_block);
        out.extend_from_slice(&self.aux_block);
        out
    }

    #[cfg(test)]
    /// Aggressive mode: clear all hash/hashtree descriptors (repack as an empty vbmeta)
    /// while keeping the chain partition chain. Equivalent to an all-empty `--flags 3` image.
    pub fn strip_descriptors(&self, flags: u32) -> Vec<u8> {
        let mut aux = Vec::new();
        // The public key stays at the start of the aux block (public_key_offset); descriptors move after it.
        let h = &self.header;
        let pk_end = (h.public_key_offset + h.public_key_size) as usize;
        let pk = if (h.public_key_size as usize) <= self.aux_block.len()
            && pk_end <= self.aux_block.len()
        {
            self.aux_block[..pk_end].to_vec()
        } else {
            Vec::new()
        };
        aux.extend_from_slice(&pk);

        // Rebuild the header: descriptor area points past the public key with length 0; signature/hash zeroed;
        // the auth block is dropped, so both block sizes must be updated (otherwise parsers overrun).
        let mut header = self.rebuild_header(flags);
        header.authentication_data_block_size = 0;
        header.auxiliary_data_block_size = aux.len() as u64;
        header.descriptors_offset = aux.len() as u64;
        header.descriptors_size = 0;
        header.hash_offset = 0;
        header.hash_size = 0;
        header.signature_offset = 0;
        header.signature_size = 0;

        let mut out = Vec::with_capacity(AVB_HEADER_SIZE + aux.len());
        out.extend_from_slice(&encode_header(&header));
        out.extend_from_slice(&aux);
        out
    }

    fn rebuild_header(&self, flags: u32) -> VbmetaHeader {
        let mut h = self.header.clone();
        h.flags = flags;
        h
    }
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
fn parse_partition_name(tag: &DescriptorTag, raw: &[u8]) -> Option<String> {
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

/// Build a minimal valid vbmeta image (header + hash descriptor list, no auth block).
///
/// Purpose: simulated devices and unit tests need an image that `VbmetaImage::parse` accepts
/// and whose flags can be modified.
/// Real devices carry a signature block; algorithm_type=0 (unsigned) is also valid for avbtool
/// (as produced by `avbtool make_vbmeta_image --flags N` without a key).
pub fn synthetic_vbmeta(flags: u32, partition_names: &[&str]) -> Vec<u8> {
    let mut aux = Vec::new();
    for name in partition_names {
        let name = name.as_bytes();
        let payload_len = 8 + 32 + 4 + 4 + 4 + 4 + name.len();
        aux.extend_from_slice(&3u64.to_be_bytes()); // tag = Hash
        aux.extend_from_slice(&(payload_len as u64).to_be_bytes());
        aux.extend_from_slice(&0x10_0000u64.to_be_bytes()); // image_size
        let mut algo = b"sha256".to_vec();
        algo.resize(32, 0);
        aux.extend_from_slice(&algo);
        aux.extend_from_slice(&(name.len() as u32).to_be_bytes());
        aux.extend_from_slice(&0u32.to_be_bytes()); // salt_len
        aux.extend_from_slice(&0u32.to_be_bytes()); // digest_len
        aux.extend_from_slice(&0u32.to_be_bytes()); // flags
        aux.extend_from_slice(name);
    }

    let h = VbmetaHeader {
        required_libavb_version_major: 1,
        required_libavb_version_minor: 0,
        authentication_data_block_size: 0,
        auxiliary_data_block_size: aux.len() as u64,
        algorithm_type: 0,
        hash_offset: 0,
        hash_size: 0,
        signature_offset: 0,
        signature_size: 0,
        public_key_offset: 0,
        public_key_size: 0,
        public_key_metadata_offset: 0,
        public_key_metadata_size: 0,
        descriptors_offset: 0,
        descriptors_size: aux.len() as u64,
        rollback_index: 0,
        flags,
        rollback_index_location: 0,
        release_string: "synthetic".to_string(),
    };
    let mut out = encode_header(&h);
    out.extend_from_slice(&aux);
    out
}

/// Encode the header in libavb byte order (used to rebuild after stripping).
fn encode_header(h: &VbmetaHeader) -> Vec<u8> {
    let mut b = Vec::with_capacity(AVB_HEADER_SIZE);
    b.extend_from_slice(AVB_MAGIC);
    b.extend_from_slice(&h.required_libavb_version_major.to_be_bytes());
    b.extend_from_slice(&h.required_libavb_version_minor.to_be_bytes());
    b.extend_from_slice(&h.authentication_data_block_size.to_be_bytes());
    b.extend_from_slice(&h.auxiliary_data_block_size.to_be_bytes());
    b.extend_from_slice(&h.algorithm_type.to_be_bytes());
    b.extend_from_slice(&h.hash_offset.to_be_bytes());
    b.extend_from_slice(&h.hash_size.to_be_bytes());
    b.extend_from_slice(&h.signature_offset.to_be_bytes());
    b.extend_from_slice(&h.signature_size.to_be_bytes());
    b.extend_from_slice(&h.public_key_offset.to_be_bytes());
    b.extend_from_slice(&h.public_key_size.to_be_bytes());
    b.extend_from_slice(&h.public_key_metadata_offset.to_be_bytes());
    b.extend_from_slice(&h.public_key_metadata_size.to_be_bytes());
    b.extend_from_slice(&h.descriptors_offset.to_be_bytes());
    b.extend_from_slice(&h.descriptors_size.to_be_bytes());
    b.extend_from_slice(&h.rollback_index.to_be_bytes());
    b.extend_from_slice(&h.flags.to_be_bytes());
    b.extend_from_slice(&h.rollback_index_location.to_be_bytes());
    let mut rel = h.release_string.as_bytes().to_vec();
    rel.resize(48, 0);
    b.extend_from_slice(&rel);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid vbmeta: header + 1 hash descriptor (inside the aux block).
    fn make_test_vbmeta() -> Vec<u8> {
        // descriptor: tag=3(Hash), len = payload size
        let name = b"boot";
        let d_payload_len = 8 + 32 + 4 + 4 + 4 + 4 + name.len();
        let mut d = Vec::new();
        d.extend_from_slice(&3u64.to_be_bytes());
        d.extend_from_slice(&(d_payload_len as u64).to_be_bytes());
        d.extend_from_slice(&0x1000u64.to_be_bytes()); // image_size
        d.extend_from_slice(&[0u8; 32]); // hash algorithm "sha256" padded? lenient test
        d.extend_from_slice(&(name.len() as u32).to_be_bytes()); // partition_name_len
        d.extend_from_slice(&0u32.to_be_bytes()); // salt_len
        d.extend_from_slice(&0u32.to_be_bytes()); // digest_len
        d.extend_from_slice(&0u32.to_be_bytes()); // flags
        d.extend_from_slice(name);

        // Aux block: public key (empty) + descriptors
        let aux = d.clone();
        // Header
        let mut hdr = Vec::new();
        hdr.extend_from_slice(AVB_MAGIC);
        hdr.extend_from_slice(&1u32.to_be_bytes()); // major
        hdr.extend_from_slice(&2u32.to_be_bytes()); // minor
        hdr.extend_from_slice(&0u64.to_be_bytes()); // auth size
        hdr.extend_from_slice(&(aux.len() as u64).to_be_bytes()); // aux size
        hdr.extend_from_slice(&0u32.to_be_bytes()); // algorithm
        hdr.extend_from_slice(&0u64.to_be_bytes()); // hash off
        hdr.extend_from_slice(&0u64.to_be_bytes()); // hash size
        hdr.extend_from_slice(&0u64.to_be_bytes()); // sig off
        hdr.extend_from_slice(&0u64.to_be_bytes()); // sig size
        hdr.extend_from_slice(&0u64.to_be_bytes()); // pk off
        hdr.extend_from_slice(&0u64.to_be_bytes()); // pk size
        hdr.extend_from_slice(&0u64.to_be_bytes()); // pk meta off
        hdr.extend_from_slice(&0u64.to_be_bytes()); // pk meta size
        hdr.extend_from_slice(&0u64.to_be_bytes()); // descriptors off = 0 (relative to aux block)
        hdr.extend_from_slice(&(aux.len() as u64).to_be_bytes());
        hdr.extend_from_slice(&0u64.to_be_bytes()); // rollback
        hdr.extend_from_slice(&0u32.to_be_bytes()); // flags
        hdr.extend_from_slice(&0u32.to_be_bytes()); // rollback loc
        let mut rel = b"test release".to_vec();
        rel.resize(48, 0);
        hdr.extend_from_slice(&rel);

        let mut out = hdr;
        out.extend_from_slice(&aux);
        out
    }

    #[test]
    fn parse_and_info() {
        let data = make_test_vbmeta();
        let v = VbmetaImage::parse(&data).unwrap();
        assert_eq!(v.header.required_libavb_version_major, 1);
        assert_eq!(v.header.required_libavb_version_minor, 2);
        assert_eq!(v.descriptors.len(), 1);
        assert_eq!(v.descriptors[0].tag, DescriptorTag::Hash);
        assert_eq!(v.descriptors[0].partition_name.as_deref(), Some("boot"));
        let info = v.info();
        assert!(info.contains("Hash: boot"));
        assert!(info.contains("Verification disabled: false"));
    }

    #[test]
    fn disable_verification_sets_flags() {
        let data = make_test_vbmeta();
        let v = VbmetaImage::parse(&data).unwrap();
        let patched = v.disable_verification(FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED);
        let v2 = VbmetaImage::parse(&patched).unwrap();
        assert_eq!(v2.header.flags, 3);
        assert_eq!(v2.descriptors.len(), 1); // descriptors preserved
        let info = v2.info();
        assert!(info.contains("Hashtree disabled: true"));
        assert!(info.contains("Verification disabled: true"));
    }

    #[test]
    fn strip_descriptors_empties_them() {
        let data = make_test_vbmeta();
        let v = VbmetaImage::parse(&data).unwrap();
        let stripped = v.strip_descriptors(3);
        let v2 = VbmetaImage::parse(&stripped).unwrap();
        assert_eq!(v2.header.flags, 3);
        assert!(v2.descriptors.is_empty());
    }

    #[test]
    fn rejects_bad_magic_and_short() {
        let mut bad = vec![0u8; AVB_HEADER_SIZE];
        bad[0..4].copy_from_slice(b"AVB1");
        assert!(matches!(
            VbmetaImage::parse(&bad).unwrap_err(),
            AvbError::BadMagic
        ));
        assert!(matches!(
            VbmetaImage::parse(&[0u8; 16]).unwrap_err(),
            AvbError::TooSmall
        ));
    }
}
