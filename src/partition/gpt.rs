//! GPT 分区表数据结构与解析
//!
//! - `PartitionEntry` — 单个分区条目
//! - `GptInfo`       — 整个 GPT 头 + 分区表解析器

/// 分区条目信息
#[derive(Debug, Clone)]
pub struct PartitionEntry {
    pub name: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub start_addr: u64,
    pub size: u64,
}

/// GPT 分区表信息
pub struct GptInfo<'a> {
    pub num_part_entries: u32,
    pub part_entry_size: u32,
    pub part_entry_start_lba: u64,
    pub revision: u32,
    pub header_size: u32,
    pub header_crc32: u32,
    pub partition_entries_crc32: u32,
    pub first_usable_lba: u64,
    pub current_lba: u64,
    data: &'a [u8],
    pub base_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GptCrcReport {
    pub stored_header_crc32: u32,
    pub calculated_header_crc32: u32,
    pub stored_partition_entries_crc32: u32,
    pub calculated_partition_entries_crc32: u32,
}

impl GptCrcReport {
    pub fn header_ok(&self) -> bool {
        self.stored_header_crc32 == self.calculated_header_crc32
    }

    pub fn partition_entries_ok(&self) -> bool {
        self.stored_partition_entries_crc32 == self.calculated_partition_entries_crc32
    }
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

impl<'a> GptInfo<'a> {
    /// 从 GPT 数据解析 GPT 信息
    pub fn parse(data: &'a [u8]) -> Result<Self, String> {
        let base_offset = data
            .windows(8)
            .position(|w| w == b"EFI PART")
            .ok_or_else(|| "GPT 数据无效".to_string())?;

        let revision =
            u32::from_le_bytes(data[base_offset + 8..base_offset + 12].try_into().unwrap());
        let header_size =
            u32::from_le_bytes(data[base_offset + 12..base_offset + 16].try_into().unwrap());
        let header_crc32 =
            u32::from_le_bytes(data[base_offset + 16..base_offset + 20].try_into().unwrap());
        let num_part_entries =
            u32::from_le_bytes(data[base_offset + 80..base_offset + 84].try_into().unwrap());
        let part_entry_size =
            u32::from_le_bytes(data[base_offset + 84..base_offset + 88].try_into().unwrap());
        let partition_entries_crc32 =
            u32::from_le_bytes(data[base_offset + 88..base_offset + 92].try_into().unwrap());
        let part_entry_start_lba =
            u64::from_le_bytes(data[base_offset + 72..base_offset + 80].try_into().unwrap());
        let first_usable_lba =
            u64::from_le_bytes(data[base_offset + 32..base_offset + 40].try_into().unwrap());
        let current_lba =
            u64::from_le_bytes(data[base_offset + 24..base_offset + 32].try_into().unwrap());

        Ok(GptInfo {
            num_part_entries,
            part_entry_size,
            part_entry_start_lba,
            revision,
            header_size,
            header_crc32,
            partition_entries_crc32,
            first_usable_lba,
            current_lba,
            data,
            base_offset,
        })
    }

    pub fn crc_report(&self) -> Result<GptCrcReport, String> {
        let header_size = self.header_size as usize;
        if self.base_offset + header_size > self.data.len() || header_size < 20 {
            return Err("GPT header size 超出数据范围".to_string());
        }

        let mut header = self.data[self.base_offset..self.base_offset + header_size].to_vec();
        header[16..20].fill(0);
        let calculated_header_crc32 = crc32_ieee(&header);

        let entries_offset = (self.part_entry_start_lba * 512) as usize;
        let entries_len = self
            .num_part_entries
            .checked_mul(self.part_entry_size)
            .ok_or("GPT 分区条目长度溢出")? as usize;
        if entries_offset + entries_len > self.data.len() {
            return Err("GPT 分区条目区域超出数据范围".to_string());
        }
        let calculated_partition_entries_crc32 =
            crc32_ieee(&self.data[entries_offset..entries_offset + entries_len]);

        Ok(GptCrcReport {
            stored_header_crc32: self.header_crc32,
            calculated_header_crc32,
            stored_partition_entries_crc32: self.partition_entries_crc32,
            calculated_partition_entries_crc32,
        })
    }

    fn parse_entry(&self, entry_offset: usize) -> Result<PartitionEntry, String> {
        if entry_offset + self.part_entry_size as usize > self.data.len() {
            return Err("分区条目数据超出范围".to_string());
        }
        let entry = &self.data[entry_offset..entry_offset + self.part_entry_size as usize];

        // GPT 分区项结构 (128 字节):
        // offset  0: type GUID (16 字节)
        // offset 16: unique GUID (16 字节)
        // offset 32: first_lba (8 字节, u64 LE)
        // offset 40: last_lba (8 字节, u64 LE)
        // offset 48: flags (8 字节)
        // offset 56: name (72 字节, UTF-16LE)
        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());

        // 解析分区名 (offset 56, 72 字节, UTF-16LE, 以 0 结尾)
        let name_offset = 56;
        let name_len = 72;
        let mut name_bytes = [0u8; 72];
        name_bytes.copy_from_slice(&entry[name_offset..name_offset + name_len]);
        let name_u16: Vec<u16> = name_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let name = String::from_utf16_lossy(&name_u16);
        let start_addr = first_lba
            .checked_mul(512)
            .ok_or("GPT start_addr overflow")?;
        let sector_count = last_lba
            .checked_sub(first_lba)
            .and_then(|d| d.checked_add(1))
            .ok_or("GPT invalid partition: last_lba < first_lba")?;
        let size = sector_count.checked_mul(512).ok_or("GPT size overflow")?;

        Ok(PartitionEntry {
            name,
            first_lba,
            last_lba,
            start_addr,
            size,
        })
    }

    /// 遍历所有分区
    pub fn iter_partitions(&self) -> impl Iterator<Item = PartitionEntry> + '_ {
        let part_entry_start_byte = (self.part_entry_start_lba * 512) as usize;
        let entry_size = self.part_entry_size as usize;
        let num_entries = self.num_part_entries as usize;

        let data_len = self.data.len();
        (0..num_entries).filter_map(move |i| {
            let entry_offset = part_entry_start_byte + i * entry_size;
            if entry_offset + entry_size > data_len {
                return None;
            }
            // 检查 type GUID 是否全零（空条目）
            if self.data[entry_offset..entry_offset + 16]
                .iter()
                .all(|&b| b == 0)
            {
                return None;
            }
            self.parse_entry(entry_offset).ok()
        })
    }

    /// 按名称查找分区（不区分大小写）
    pub fn find_partition(&self, name: &str) -> Option<PartitionEntry> {
        self.iter_partitions()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// 获取所有分区列表
    pub fn partitions(&self) -> Vec<PartitionEntry> {
        self.iter_partitions().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_gpt_with_entry(type_guid_nonzero: bool, unique_guid_nonzero: bool) -> Vec<u8> {
        let mut data = vec![0u8; 1536];
        let header = 512;
        data[header..header + 8].copy_from_slice(b"EFI PART");
        data[header + 8..header + 12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        data[header + 12..header + 16].copy_from_slice(&92u32.to_le_bytes());
        data[header + 24..header + 32].copy_from_slice(&1u64.to_le_bytes());
        data[header + 32..header + 40].copy_from_slice(&34u64.to_le_bytes());
        data[header + 72..header + 80].copy_from_slice(&2u64.to_le_bytes());
        data[header + 80..header + 84].copy_from_slice(&1u32.to_le_bytes());
        data[header + 84..header + 88].copy_from_slice(&128u32.to_le_bytes());

        let entry = 1024;
        if type_guid_nonzero {
            data[entry] = 1;
        }
        if unique_guid_nonzero {
            data[entry + 16] = 1;
        }
        data[entry + 32..entry + 40].copy_from_slice(&64u64.to_le_bytes());
        data[entry + 40..entry + 48].copy_from_slice(&127u64.to_le_bytes());
        data[entry + 56..entry + 58].copy_from_slice(&('x' as u16).to_le_bytes());

        let entries_crc = crc32_ieee(&data[1024..1152]);
        data[header + 88..header + 92].copy_from_slice(&entries_crc.to_le_bytes());

        let mut header_bytes = data[header..header + 92].to_vec();
        header_bytes[16..20].fill(0);
        let header_crc = crc32_ieee(&header_bytes);
        data[header + 16..header + 20].copy_from_slice(&header_crc.to_le_bytes());
        data
    }

    #[test]
    fn crc32_ieee_matches_standard_vector() {
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn gpt_crc_report_validates_header_and_entries() {
        let data = build_minimal_gpt_with_entry(true, true);
        let gpt = GptInfo::parse(&data).unwrap();
        let report = gpt.crc_report().unwrap();

        assert!(report.header_ok());
        assert!(report.partition_entries_ok());
    }

    #[test]
    fn partition_iteration_skips_empty_type_guid_not_empty_unique_guid() {
        let with_zero_unique = build_minimal_gpt_with_entry(true, false);
        let gpt = GptInfo::parse(&with_zero_unique).unwrap();
        assert_eq!(gpt.partitions().len(), 1);

        let with_zero_type = build_minimal_gpt_with_entry(false, true);
        let gpt = GptInfo::parse(&with_zero_type).unwrap();
        assert_eq!(gpt.partitions().len(), 0);
    }
}
