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
    pub first_usable_lba: u64,
    pub current_lba: u64,
    data: &'a [u8],
    pub base_offset: usize,
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
        let num_part_entries =
            u32::from_le_bytes(data[base_offset + 80..base_offset + 84].try_into().unwrap());
        let part_entry_size =
            u32::from_le_bytes(data[base_offset + 84..base_offset + 88].try_into().unwrap());
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
            first_usable_lba,
            current_lba,
            data,
            base_offset,
        })
    }

    fn parse_entry(&self, entry_offset: usize) -> Result<PartitionEntry, String> {
        if entry_offset + self.part_entry_size as usize > self.data.len() {
            return Err("分区条目数据超出范围".to_string());
        }
        let entry = &self.data[entry_offset..entry_offset + self.part_entry_size as usize];

        // 解析分区名 (前 72 字节, UTF-16LE, 以 0 结尾)
        let mut name_bytes = [0u8; 72];
        name_bytes.copy_from_slice(&entry[0..72]);
        let name_u16: Vec<u16> = name_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let name = String::from_utf16_lossy(&name_u16);

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let start_addr = first_lba
            .checked_mul(512)
            .ok_or("GPT start_addr overflow")?;
        let sector_count = last_lba
            .checked_sub(first_lba)
            .and_then(|d| d.checked_add(1))
            .ok_or("GPT invalid partition: last_lba < first_lba")?;
        let size = sector_count
            .checked_mul(512)
            .ok_or("GPT size overflow")?;

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
            if self.data[entry_offset + 16..entry_offset + 32]
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
