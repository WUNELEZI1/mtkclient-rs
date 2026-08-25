//! GPT CRC 校验报告日志

use crate::partition::GptInfo;
use log::{debug, warn};

pub(crate) fn log_gpt_crc_report(gpt_info: &GptInfo<'_>) {
    match gpt_info.crc_report() {
        Ok(report) => {
            if report.header_ok() {
                debug!(
                    "GPT Header CRC 校验成功: 0x{:08X}",
                    report.stored_header_crc32
                );
            } else {
                warn!(
                    "GPT Header CRC 校验失败: 原CRC=0x{:08X}, 计算CRC=0x{:08X}",
                    report.stored_header_crc32, report.calculated_header_crc32
                );
            }

            if report.partition_entries_ok() {
                debug!(
                    "GPT 分区条目 CRC 校验成功: 0x{:08X}",
                    report.stored_partition_entries_crc32
                );
            } else {
                warn!(
                    "GPT 分区条目 CRC 校验失败: 原CRC=0x{:08X}, 计算CRC=0x{:08X}",
                    report.stored_partition_entries_crc32,
                    report.calculated_partition_entries_crc32
                );
            }
        }
        Err(e) => warn!("GPT CRC 校验跳过: {}", e),
    }
}
