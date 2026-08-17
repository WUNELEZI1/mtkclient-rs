use crate::color::Colorize;

/// 打印完整 EMMC 信息（对齐 C# 版输出格式）
pub fn print_emmc_info(info: &crate::da::EmmcInfo) {
    // 格式化字节数为人类可读字符串
    fn fmt_bytes(b: u64) -> String {
        if b >= 1_073_741_824 {
            format!("{:.2} GB", b as f64 / 1_073_741_824.0)
        } else if b >= 1_048_576 {
            format!("{:.2} MB", b as f64 / 1_048_576.0)
        } else if b >= 1024 {
            format!("{:.2} KB", b as f64 / 1024.0)
        } else {
            format!("{} B", b)
        }
    }

    // 将 CID 字节数组格式化为十六进制字符串（对齐 C# 版）
    fn fmt_cid_hex(cid: &[u8]) -> String {
        cid.iter().map(|b| format!("{:02X}", b)).collect()
    }

    // 每列先格式化定宽字符串，再整体上色，避免 ANSI 破坏对齐
    let mut rows: Vec<(String, String, String)> = Vec::new();

    // Type
    rows.push((
        format!("{:<15}", "EMMC Type"),
        info.emmc_type.clone(),
        String::new(),
    ));

    // User Area
    if info.user_size > 0 {
        rows.push((
            format!("{:<15}", "EMMC USER Size"),
            format!("0x{:X} ({})", info.user_size, fmt_bytes(info.user_size)),
            String::new(),
        ));
    }

    // Boot1
    if info.boot1_size > 0 {
        rows.push((
            format!("{:<15}", "EMMC Boot1 Size"),
            format!("0x{:X} ({})", info.boot1_size, fmt_bytes(info.boot1_size)),
            String::new(),
        ));
    }

    // Boot2
    if info.boot2_size > 0 {
        rows.push((
            format!("{:<15}", "EMMC Boot2 Size"),
            format!("0x{:X} ({})", info.boot2_size, fmt_bytes(info.boot2_size)),
            String::new(),
        ));
    }

    // RPMB
    if info.rpmb_size > 0 {
        rows.push((
            format!("{:<15}", "EMMC RPMB Size"),
            format!("0x{:X} ({})", info.rpmb_size, fmt_bytes(info.rpmb_size)),
            String::new(),
        ));
    }

    // Block Size
    if info.block_size > 0 {
        rows.push((
            format!("{:<15}", "Block Size"),
            format!("0x{:X} ({} bytes)", info.block_size, info.block_size),
            String::new(),
        ));
    }

    // CID（十六进制格式，对齐 C# 版）
    if !info.cid.is_empty() {
        let cid_hex = fmt_cid_hex(&info.cid);
        rows.push((format!("{:<15}", "EMMC CID"), cid_hex, String::new()));
    }

    // 计算最大内容宽度（不包含标签列的空格）
    let max_val_w = rows.iter().map(|(_, v, _)| v.len()).max().unwrap_or(20);
    let total_inner = 15 + 2 + max_val_w; // "EMMC Boot1 Size" + "  " + value

    println!();
    // 标题行
    let title = " eMMC Info ".to_string();
    let total_w = total_inner.max(title.len() + 2);
    let pad = total_w - title.len();
    let lpad = pad / 2;
    let rpad = pad - lpad;
    println!("+{}+", "-".repeat(total_w));
    println!(
        "|{}{}{}|",
        " ".repeat(lpad),
        title.green().bold(),
        " ".repeat(rpad)
    );
    println!("+{}+", "-".repeat(total_w));

    for (label, value, _extra) in &rows {
        let padded_val = format!("{:<width$}", value, width = max_val_w);
        println!("| {}  {} |", label.green().bold(), padded_val.green());
    }

    println!("+{}+", "-".repeat(total_w));
    println!();
}

/// 格式化字节数为人类可读字符串 (KB/MB/GB, 保留 2 位小数)
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bytes >= (1024 * 1024 * 1024) {
        format!("{:.2} GB", bytes as f64 / GB)
    } else if bytes >= (1024 * 1024) {
        format!("{:.2} MB", bytes as f64 / MB)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// 格式化字节数为带千分位的字符串
pub fn format_bytes_comma(bytes: u64) -> String {
    let s = bytes.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}
