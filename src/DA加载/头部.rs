//! MTK AllInOne DA 文件头解析（纯数据处理，无 USB 通信）
//!
//! - `DaRegion`         — DA 文件中单个 region 的元数据
//! - `parse_da_regions` — 从 DA header 区域解析 region 列表
//! - `parse_da_header`  — 解析整个 AllInOne DA 文件

use log::{info, warn};

/// DA 文件 region 结构
#[derive(Debug, Clone)]
pub(crate) struct DaRegion {
    pub(crate) buf_offset: u32, // 在文件中的偏移
    pub(crate) len: u32,        // 大小
    pub(crate) start_addr: u32, // 加载地址
    pub(crate) sig_len: u32,    // 签名长度
}

/// 从 DA header 数据中解析 region 信息
pub(crate) fn parse_da_regions(
    header: &[u8],
    region_start: usize,
    entry_region_count: u16,
) -> Vec<DaRegion> {
    let mut regions = Vec::new();

    for j in 0..entry_region_count as usize {
        let region_offset = region_start + j * 20;
        if region_offset + 20 > header.len() {
            break;
        }

        let buf_offset = u32::from_le_bytes([
            header[region_offset],
            header[region_offset + 1],
            header[region_offset + 2],
            header[region_offset + 3],
        ]);
        let len = u32::from_le_bytes([
            header[region_offset + 4],
            header[region_offset + 5],
            header[region_offset + 6],
            header[region_offset + 7],
        ]);
        let start_addr = u32::from_le_bytes([
            header[region_offset + 8],
            header[region_offset + 9],
            header[region_offset + 10],
            header[region_offset + 11],
        ]);
        let sig_len = u32::from_le_bytes([
            header[region_offset + 16],
            header[region_offset + 17],
            header[region_offset + 18],
            header[region_offset + 19],
        ]);

        regions.push(DaRegion {
            buf_offset,
            len,
            start_addr,
            sig_len,
        });
    }

    regions
}

// 解析 MTK AllInOne DA 文件
pub(crate) fn parse_da_header(
    da_data: &[u8],
    target_hw_code: u16,
) -> Result<(u32, Vec<DaRegion>, bool), String> {
    if da_data.len() < 0x6C {
        return Err("DA 文件太小，无法解析头".to_string());
    }

    // 检查是否是 MTK AllInOne DA 格式
    let header = &da_data[0..0x68];
    let is_allinone = header.contains(&0x4D) && header.contains(&0x54) && header.contains(&0x4B);

    if is_allinone {
        // 读取 DA 数量
        let count_da =
            u32::from_le_bytes([da_data[0x68], da_data[0x69], da_data[0x6A], da_data[0x6B]]);

        // 检查是否是 v6 格式
        let is_v6 = header.windows(8).any(|window| window == b"MTK_DA_v6");

        // 检查是否是旧版加载器
        let old_ldr: bool;
        let offset: usize;

        if da_data.len() > 0x6C + 0xD8 + 1 {
            let marker = [da_data[0x6C + 0xD8], da_data[0x6C + 0xD8 + 1]];
            if marker == [0xDA, 0xDA] {
                offset = 0xD8;
                old_ldr = true;
            } else {
                offset = 0xDC;
                old_ldr = false;
            }
        } else {
            offset = 0xDC;
            old_ldr = false;
        }

        // 查找适合目标 HW Code 的 DA
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];

            // 读取 DA 信息
            let magic = u16::from_le_bytes([da_header[0], da_header[1]]);
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);
            let _hw_sub_code = u16::from_le_bytes([da_header[4], da_header[5]]);
            let _hw_version = u16::from_le_bytes([da_header[6], da_header[7]]);

            // 新版加载器有 sw_version + reserved1 (4 字节)
            let _sw_version: u16;
            let _page_size: u16;
            let _entry_region_index: u16;
            let entry_region_count: u16;
            let region_start: usize;

            if old_ldr {
                _sw_version = 0;
                _page_size = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved = u16::from_le_bytes([da_header[10], da_header[11]]);
                _entry_region_index = u16::from_le_bytes([da_header[12], da_header[13]]);
                entry_region_count = u16::from_le_bytes([da_header[14], da_header[15]]);
                region_start = 16;
            } else {
                _sw_version = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved1 = u16::from_le_bytes([da_header[10], da_header[11]]);
                _page_size = u16::from_le_bytes([da_header[12], da_header[13]]);
                let _reserved3 = u16::from_le_bytes([da_header[14], da_header[15]]);
                _entry_region_index = u16::from_le_bytes([da_header[16], da_header[17]]);
                entry_region_count = u16::from_le_bytes([da_header[18], da_header[19]]);
                region_start = 20;
            }

            // 跳过不匹配的 DA，静默处理
            if hw_code != target_hw_code {
                continue;
            }

            info!("找到匹配的 DA {}，HW Code: 0x{:04X}", i, hw_code);

            let regions = parse_da_regions(da_header, region_start, entry_region_count);

            return Ok((magic as u32, regions, is_v6));
        }

        // 如果没有找到匹配的 HW Code，返回第一个有效的 DA（hw_code 不为 0）
        warn!(
            "未找到 HW Code 0x{:04X} 的 DA，尝试使用第一个有效的 DA",
            target_hw_code
        );
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);

            if hw_code != 0 {
                let region_start = da_offset + 16;
                let entry_region_count = if offset >= 16 {
                    u16::from_le_bytes([da_header[14], da_header[15]])
                } else {
                    0
                };

                let mut regions = parse_da_regions(da_header, region_start, entry_region_count);

                // 如果没有读取到 regions，使用默认的 Stage1 和 Stage2 region
                if regions.is_empty() {
                    warn!("备选 DA 没有 region 信息，使用默认值");
                    // 默认 Stage1 region
                    regions.push(DaRegion {
                        buf_offset: 0x376C,
                        len: 0x270,
                        start_addr: 0x200000,
                        sig_len: 0x0,
                    });
                    // 默认 Stage2 region
                    regions.push(DaRegion {
                        buf_offset: 0x39E4,
                        len: 0xE660,
                        start_addr: 0x80000000,
                        sig_len: 0x100,
                    });
                }

                info!("使用 DA {} (HW Code: 0x{:04X}) 作为备选", i, hw_code);
                return Ok((0, regions, is_v6));
            }
        }

        Err("在 AllInOne DA 中未找到有效的 DA 配置".to_string())
    } else {
        // 传统 DA 格式解析
        let magic = u32::from_le_bytes([da_data[0], da_data[1], da_data[2], da_data[3]]);
        let hw_code = u16::from_le_bytes([da_data[4], da_data[5]]);
        let entry_region_count = u16::from_le_bytes([da_data[16], da_data[17]]);

        if hw_code != target_hw_code {
            warn!(
                "DA HW Code (0x{:04X}) 与目标 (0x{:04X}) 不匹配",
                hw_code, target_hw_code
            );
        }

        let is_v6 = magic == 0x5644415F;

        let regions = parse_da_regions(da_data, 0x6C, entry_region_count);

        Ok((magic, regions, is_v6))
    }
}
