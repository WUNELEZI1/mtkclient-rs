//! GPT 分区表格式化输出
//!
//! - `print_gpt_table` — ASCII 表格风格打印分区表

use colored::Colorize;

use super::emmc;
use crate::partition::GptInfo;

/// 打印 GPT 表格到控制台（含 eMMC_Boot1/Boot2 显示）
/// 使用 ASCII 表格风格: | 和 - 作为分隔符，列标签中文
pub fn print_gpt_table(data: &[u8], boot1_size: u64, boot2_size: u64) {
    let gpt_info = match GptInfo::parse(data) {
        Ok(info) => info,
        Err(_) => return,
    };

    let partitions = gpt_info.partitions();

    // 列宽定义（地址按 0x%016X = 18 字符，含 0x 前缀）
    const W_IDX: usize = 4; // "01" 等
    const W_NAME: usize = 22; // 分区名
    const W_ADDR: usize = 18; // 0x00000000000000
    const W_SIZE: usize = 10; // "1024.00 MB"
    const W_BYTES: usize = 17; // "1,073,741,824"

    // 构建分隔线: +----+--------+...
    let sep = format!(
        "+-{}-+-{}-+-{}-+-{}-+-{}-+-{}-+",
        "-".repeat(W_IDX),
        "-".repeat(W_NAME),
        "-".repeat(W_ADDR),
        "-".repeat(W_ADDR),
        "-".repeat(W_SIZE),
        "-".repeat(W_BYTES)
    );

    // 表头行（先格式化定宽纯文本，再整体上色）
    let header_plain = format!(
        "| {:<idx$} | {:<name$} | {:>addr$} | {:>addr$} | {:>size$} | {:>bytes$} |",
        "编号",
        "名称",
        "起始地址",
        "结束地址",
        "大小",
        "字节数",
        idx = W_IDX,
        name = W_NAME,
        addr = W_ADDR,
        size = W_SIZE,
        bytes = W_BYTES,
    );

    // 格式化一行为定宽纯文本（不含颜色代码）
    fn fmt_row(idx: &str, name: &str, start: &str, end: &str, size: &str, bytes: &str) -> String {
        format!(
            "| {:<4} | {:<22} | {:>18} | {:>18} | {:>10} | {:>17} |",
            idx, name, start, end, size, bytes
        )
    }

    println!();
    println!("{}", sep);
    println!("{}", header_plain.bright_white().bold());
    println!("{}", sep);

    let mut row = 0usize;

    // eMMC Boot 区域（灰色显示，标记为 eMMC_Boot1 / eMMC_Boot2）
    if boot1_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot1",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot1_size.saturating_sub(1)),
            &emmc::format_size(boot1_size),
            &emmc::format_bytes_comma(boot1_size),
        );
        println!("{}", line.dimmed());
    }
    if boot2_size > 0 {
        row += 1;
        let line = fmt_row(
            &format!("{:02}", row),
            "eMMC_Boot2",
            &format!("0x{:016X}", 0u64),
            &format!("0x{:016X}", boot2_size.saturating_sub(1)),
            &emmc::format_size(boot2_size),
            &emmc::format_bytes_comma(boot2_size),
        );
        println!("{}", line.dimmed());
    }

    // GPT 分区（交替亮度，先 format 定宽再整体上色）
    for (i, entry) in partitions.iter().enumerate() {
        row += 1;
        let start_addr = entry.start_addr;
        let end_addr = start_addr.saturating_add(entry.size).saturating_sub(1);
        let line = fmt_row(
            &format!("{:02}", row),
            &entry.name,
            &format!("0x{:016X}", start_addr),
            &format!("0x{:016X}", end_addr),
            &emmc::format_size(entry.size),
            &emmc::format_bytes_comma(entry.size),
        );

        if i % 2 == 0 {
            println!("{}", line);
        } else {
            println!("{}", line.bright_white());
        }
    }

    // 底部分隔线 + 总计
    println!("{}", sep);
    let total_label = format!("| 共 {} 个分区", row);
    let sep_len = sep.len();
    let total_text = format!("{:<width$} |", total_label, width = sep_len - 1);
    println!("{}", total_text.green().bold());
    println!("{}", sep);
    println!();
}
