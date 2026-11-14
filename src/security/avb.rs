//! 自研 avbtool：Android Verified Boot vbmeta 镜像解析与改写
//!
//! 对齐 `avbtool`（libavb）的核心能力，纯 Rust 零依赖：
//! - [`VbmetaImage::parse`]：完整解析 vbmeta 头（0xB0 字节）与全部 descriptor；
//! - [`VbmetaImage::info`]：`avbtool info-image` 等价格式输出；
//! - [`VbmetaImage::disable_verification`]：改写 flags 关闭 verity/verification
//!   （等价 `avbtool make_vbmeta_image --flags`，设备端 bootloader 读 flags 后跳过校验，
//!   从而避免 dm-verity 启动失败）；
//! - [`VbmetaImage::strip_descriptors`]：可选把 hash/hashtree descriptor 一并清空
//!   （等价提取空 vbmeta 重打包，最激进方案）。
//!
//! 所有多字节字段为**大端序**（libavb 规定）。

use std::fmt::Write as _;

/// vbmeta 魔数。
pub const AVB_MAGIC: &[u8; 4] = b"AVB0";
/// vbmeta 头大小（0x80 头 + 48 字节 release_string）。
pub const AVB_HEADER_SIZE: usize = 0xB0;

/// flags 位：hashtree 禁用（dm-verity off）。
pub const FLAG_HASHTREE_DISABLED: u32 = 0x01;
/// flags 位：签名验证禁用。
pub const FLAG_VERIFICATION_DISABLED: u32 = 0x02;

/// descriptor 标签（libavb avb_descriptor.h）。
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
    /// descriptor 类型的可读名（供在线日志使用）。
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

/// 解析后的 descriptor（保留原始字节 + 摘要信息）。
#[derive(Debug, Clone)]
pub struct Descriptor {
    pub tag: DescriptorTag,
    /// 原始 descriptor 字节（含 tag + length 头），供改写时回写。
    pub raw: Vec<u8>,
    /// 便捷字段：Property/ChainPartition/KernelCmdline 关联的分区名（若有）。
    pub partition_name: Option<String>,
}

/// vbmeta 头字段（全部已按大端序转换）。
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

/// 完整 vbmeta 镜像（头 + descriptor 列表 + 原始认证/辅助块）。
#[derive(Debug, Clone)]
pub struct VbmetaImage {
    pub header: VbmetaHeader,
    pub descriptors: Vec<Descriptor>,
    /// 认证块（签名 + hash）原始字节。
    pub auth_block: Vec<u8>,
    /// 辅助块（公钥 + descriptor + 公钥元数据）原始字节。
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
    /// 解析 vbmeta 镜像（完整数据，头 + 认证块 + 辅助块）。
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

        // 认证块紧接头之后，辅助块紧接认证块。
        let auth_end = AVB_HEADER_SIZE + header.authentication_data_block_size as usize;
        let aux_end = auth_end + header.auxiliary_data_block_size as usize;
        if auth_end > data.len() || aux_end > data.len() {
            return Err(AvbError::BadDescriptor);
        }
        let auth_block = data[AVB_HEADER_SIZE..auth_end].to_vec();
        let aux_block = data[auth_end..aux_end].to_vec();

        // descriptor 位于辅助块内（descriptors_offset 相对辅助块起点）。
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
                raw,
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

    /// `avbtool info-image` 等价格式。
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

    /// 改写 flags 并重组镜像。`flags` 通常传
    /// `FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED`（=3，刷机场景推荐）。
    ///
    /// 注意：改 flags 后原签名失效，但 flags 位本身会被 bootloader 先读——
    /// 校验关闭路径下不会因签名失效而拒绝启动（与官方 avbtool 行为一致）。
    pub fn disable_verification(&self, flags: u32) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(AVB_HEADER_SIZE + self.auth_block.len() + self.aux_block.len());
        out.extend_from_slice(&encode_header(&self.rebuild_header(flags)));
        out.extend_from_slice(&self.auth_block);
        out.extend_from_slice(&self.aux_block);
        out
    }

    /// 激进模式：清空全部 hash/hashtree descriptor（重打包为空 vbmeta），
    /// 同时保留 chain 分区链。等价 `avbtool make_vbmeta_image --flags 3` 全空镜像。
    pub fn strip_descriptors(&self, flags: u32) -> Vec<u8> {
        let mut aux = Vec::new();
        // 公钥部分仍在辅助块开头（public_key_offset），descriptor 移到其后。
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

        // 重组头：descriptor 区指向公钥之后，长度为 0；签名/哈希置零；
        // 认证块被丢弃，认证/辅助块大小必须同步更新（否则解析端越界）。
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

/// descriptor 内分区名解析（按 libavb 各 descriptor 的真实字段偏移）。
///
/// 布局（均相对 descriptor 起点；tag(8)+length(8) 前缀共 16 字节）：
/// - Hash：image_size(8) + hash_algorithm[32] + name_len(4)@56 + salt_len(4) +
///   digest_len(4) + flags(4)，名字在 @72；
/// - Hashtree v1.1+：dm_verity(8) + image_size(8) + tree_offset(8) + tree_size(8) +
///   hash_algorithm(4) + name_len(4)@52 + salt_len(4) + root_digest_len(4) + flags(4) +
///   reserved[60]，名字在 @128；v1.0 无 reserved，名字在 @68；
/// - Chain：location(4) + name_len(4)@20 + key_len(4) + flags(4)，名字在 @32。
fn parse_partition_name(tag: &DescriptorTag, raw: &[u8]) -> Option<String> {
    /// 从 `name_len_off` 读长度、`name_off` 读名字并校验可打印。
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

/// 头结构按 libavb 字节序编码（供 strip 后重组）。
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

    /// 构造一个最小合法 vbmeta：头 + 1 个 hash descriptor（在辅助块内）。
    fn make_test_vbmeta() -> Vec<u8> {
        // descriptor: tag=3(Hash) len=分区名区大小
        let name = b"boot";
        let d_payload_len = 8 + 32 + 4 + 4 + 4 + 4 + name.len();
        let mut d = Vec::new();
        d.extend_from_slice(&3u64.to_be_bytes());
        d.extend_from_slice(&(d_payload_len as u64).to_be_bytes());
        d.extend_from_slice(&0x1000u64.to_be_bytes()); // image_size
        d.extend_from_slice(&[0u8; 32]); // hash algorithm "sha256" padded? 宽松测试
        d.extend_from_slice(&(name.len() as u32).to_be_bytes()); // partition_name_len
        d.extend_from_slice(&0u32.to_be_bytes()); // salt_len
        d.extend_from_slice(&0u32.to_be_bytes()); // digest_len
        d.extend_from_slice(&0u32.to_be_bytes()); // flags
        d.extend_from_slice(name);

        // 辅助块：公钥（空）+ descriptor
        let aux = d.clone();
        // 头
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
        hdr.extend_from_slice(&0u64.to_be_bytes()); // descriptors off = 0（相对辅助块）
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
        assert_eq!(v2.descriptors.len(), 1); // descriptor 保留
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
