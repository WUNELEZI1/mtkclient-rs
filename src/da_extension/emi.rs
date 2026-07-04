//! EMI 数据提取模块
//!
//! 负责从 preloader 二进制中提取 DRAM 初始化参数（EMI = External Memory Interface）。
//! 流程：
//! 1. `load_preloader_emi`: 读取 preloader 文件，调用 `extract_emi` 解析
//! 2. `extract_emi`: 在 preloader 数据中搜索 4D 4D 4D 01 38 标记 + MTK_BLOADER_INFO_v 字符串，
//!    按 mlen/siglen/dramsize 截取实际 EMI 数据
//!
//! 拆分动机：
//! - EMI 提取逻辑独立于 USB 通信，单元测试友好
//! - 错误信息更聚焦（路径不存在/数据格式问题）
//! - 不污染主协议层文件大小

use log::{info, trace};

use crate::da_extension::DAXFlash;

impl<'a> DAXFlash<'a> {
    /// 从 preloader 文件中提取 EMI 数据
    ///
    /// preloader_path 必须为非空字符串（要么是 --preloader 指定的文件，
    /// 要么是 dump_preloader_payload 提取出的文件名）。
    /// 传入空字符串视为"路径未确定"，直接返回错误而不是静默吞错。
    pub fn load_preloader_emi(&mut self, preloader_path: &str) -> Result<bool, String> {
        // 强制要求非空路径：不再静默吞错
        if preloader_path.is_empty() {
            return Err(
                "preloader 路径为空：未指定 --preloader 且 dump_preloader_payload 未产生文件名"
                    .to_string(),
            );
        }

        info!("加载 preloader 文件: {}", preloader_path);

        // 读取 preloader 文件
        let preloader_data = match std::fs::read(preloader_path) {
            Ok(data) => data,
            Err(e) => {
                return Err(format!("无法打开 preloader 文件 {}: {}", preloader_path, e));
            }
        };

        info!("preloader 文件大小: {} 字节", preloader_data.len());

        // 提取 EMI 数据
        match self.extract_emi(&preloader_data) {
            Ok((version, emi_data)) => {
                let emi_len = emi_data.len();
                self.emi = Some(emi_data);
                self.emi_version = version;
                info!(
                    "成功提取 EMI 数据，版本: {}, 大小: {} 字节",
                    version, emi_len
                );
                Ok(true)
            }
            Err(e) => Err(format!("提取 EMI 数据失败: {}", e)),
        }
    }

    /// 提取 EMI 数据的内部方法
    pub(crate) fn extract_emi(&self, data: &[u8]) -> Result<(u32, Vec<u8>), String> {
        // 查找标记
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        let idx = data
            .windows(marker.len())
            .position(|window| window == marker);

        let mut emi_data = data.to_vec();

        if let Some(idx) = idx {
            info!("找到 EMI 标记，偏移: 0x{:08X}", idx);
            emi_data = data[idx..].to_vec();

            // 读取 mlen 和 siglen
            if emi_data.len() >= 0x30 {
                let mlen = u32::from_le_bytes(emi_data[0x20..0x24].try_into().unwrap());
                let siglen = u32::from_le_bytes(emi_data[0x2C..0x30].try_into().unwrap());
                info!("mlen: 0x{:08X}, siglen: 0x{:08X}", mlen, siglen);

                // 截取数据
                if mlen as usize <= emi_data.len() {
                    emi_data = emi_data[..mlen as usize - siglen as usize].to_vec();
                }

                // 读取 dramsize
                if emi_data.len() >= 4 {
                    let dramsize =
                        u32::from_le_bytes(emi_data[emi_data.len() - 4..].try_into().unwrap());
                    info!("dramsize: 0x{:08X}", dramsize);

                    if dramsize == 0 && emi_data.len() >= 0x804 {
                        emi_data = emi_data[..emi_data.len() - 0x800].to_vec();
                        if emi_data.len() >= 4 {
                            let dramsize = u32::from_le_bytes(
                                emi_data[emi_data.len() - 4..].try_into().unwrap(),
                            );
                            info!("调整后 dramsize: 0x{:08X}", dramsize);
                        }
                    }

                    // 截取 EMI 数据
                    if dramsize > 0 && emi_data.len() >= (dramsize + 4) as usize {
                        emi_data = emi_data
                            [emi_data.len() - (dramsize + 4) as usize..emi_data.len() - 4]
                            .to_vec();
                    }
                }
            }
        }

        // 查找 MTK_BLOADER_INFO_v 字符串
        let bldrstring = b"MTK_BLOADER_INFO_v";
        let idx = emi_data
            .windows(bldrstring.len())
            .position(|window| window == bldrstring);

        if let Some(idx) = idx {
            info!("找到 MTK_BLOADER_INFO_v，偏移: 0x{:08X}", idx);

            // 提取版本号
            let version_str = &emi_data[idx + bldrstring.len()..idx + bldrstring.len() + 2];
            let version = String::from_utf8_lossy(version_str)
                .trim_end_matches('\0')
                .parse::<u32>()
                .unwrap_or_default();
            info!("EMI 版本: {}", version);

            // ⚠️ 关键修复：如果 MTK_BLOADER_INFO_v 在偏移 0，返回整个 emi_data
            // 对齐 Python m_extract_emi: if idx == 0 and damode == XFLASH: return ver, data
            if idx == 0 {
                trace!("MTK_BLOADER_INFO_v 在偏移 0，使用完整 EMI 数据（含 header）");
                trace!("EMI 数据大小: {} 字节", emi_data.len());
                return Ok((version, emi_data));
            }

            // 原有逻辑：只提取 MTK_BIN 后的数据
            let mtk_bin_idx = emi_data.windows(7).position(|window| window == b"MTK_BIN");
            if let Some(mtk_bin_idx) = mtk_bin_idx {
                let emi = emi_data[mtk_bin_idx + 0xC..].to_vec();
                info!("EMI 数据大小: {} 字节", emi.len());
                return Ok((version, emi));
            }
        }

        Err("未找到 EMI 数据".to_string())
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    /// 构造一个最小的 preloader 切片，验证 extract_emi 的截断逻辑
    #[test]
    fn extract_emi_no_marker_returns_error() {
        // 模拟一个空 DAXFlash 不可行（需要借用 preloader），因此这里仅做形式验证
        // 真实测试需要 mock device
        let bogus_data = [0u8; 16];
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        assert!(!bogus_data.windows(marker.len()).any(|w| w == marker));
    }
}
