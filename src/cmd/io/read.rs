use crate::color::Colorize;
use crate::da::DAXFlash;
use log::info;

/// Read partition data into a file
///
/// Usage:
///   mtkclient r `<part>` `<file>`                     → read a physical/logical partition
///   mtkclient r super --dp <logical_part> `<file>`   → read a dynamic partition inside super
///
/// --dp examples:
///   mtkclient r super --dp system system.img       → read system inside super
///   mtkclient r super --dp vendor vendor.img       → read vendor inside super
pub fn cmd_read(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err(
            "用法: mtkclient r <part> <file> 或 mtkclient r super --dp <logical_part> <file>"
                .into(),
        );
    }

    // Parse the --dp argument: r super --dp system output.img
    let dp_idx = args.iter().position(|s| s == "--dp");
    if let Some(idx) = dp_idx {
        // Dynamic partition mode
        if args[0].to_lowercase() != "super" {
            return Err("--dp 只能与 super 分区一起使用".into());
        }
        let logical_name = args.get(idx + 1).ok_or("--dp 后缺少逻辑分区名")?;
        let output_file = args.get(idx + 2).ok_or("缺少输出文件名")?;

        info!(
            "读取 super 内的动态分区: {} → {}",
            logical_name, output_file
        );
        da.read_dynamic_partition(logical_name, output_file)
            .map_err(|e| format!("读取动态分区失败: {}", e))?;
        info!(
            "{}",
            format!("super[{}] -> {}", logical_name, output_file).green()
        );
        return Ok(());
    }

    // Regular partition read
    da.read_partition(&args[0], &args[1])
        .map_err(|e| format!("读取失败: {}", e))?;
    info!("{}", format!("{} -> {}", args[0], args[1]).green());
    Ok(())
}

/// Read device memory and print a hex dump
///
/// Usage:
///   peek `<addr>`          — read 4 bytes (default)
///   peek `<addr>` `<size>`   — read the given number of bytes
///
/// The address accepts a 0x prefix or decimal format
pub fn cmd_peek(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: peek <addr> [size]".into());
    }

    let addr = parse_addr(&args[0])?;
    let size: u32 = if args.len() >= 2 {
        parse_size(&args[1])?
    } else {
        4 // 4 bytes by default
    };

    if size == 0 {
        return Err("size 不能为 0".into());
    }
    if size > 0x100000 {
        return Err("size 不能超过 1MB（peek 适用于小范围内存读取）".into());
    }

    let data = da
        .cmd_peek(addr, size)
        .map_err(|e| format!("peek 失败: {}", e))?;

    println!(
        "{}",
        format!("[peek] 地址 0x{:08X}, 读取 {} 字节:", addr, data.len()).cyan()
    );
    println!("{}", crate::util::hex_dump(&data, addr));

    Ok(())
}

/// Write data into device memory
///
/// Usage:
///   poke `<addr>` <hex_data>
///
/// The address accepts a 0x prefix or decimal format
/// hex_data accepts: AABB, AA BB, AA:BB, 0xAABB, etc.
pub fn cmd_poke(da: &mut DAXFlash, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: poke <addr> <hex_data>".into());
    }

    let addr = parse_addr(&args[0])?;
    let data = crate::util::parse_hex(&args[1]).map_err(|e| format!("hex 数据解析失败: {}", e))?;

    if data.is_empty() {
        return Err("hex 数据为空".into());
    }

    info!(
        "写入 {} 字节到地址 0x{:08X}: {}",
        data.len(),
        addr,
        crate::util::hex_str(&data)
    );

    da.cmd_poke(addr, &data)
        .map_err(|e| format!("poke 失败: {}", e))?;

    info!(
        "{}",
        format!("已成功写入 {} 字节到 0x{:08X}", data.len(), addr).green()
    );

    Ok(())
}

/// Parse an address argument (hex with a 0x prefix, or decimal)
pub fn parse_addr(s: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map_err(|e| format!("无效的十六进制地址 '{}': {}", s, e).into())
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("无效的地址 '{}': {}", s, e).into())
    }
}

/// Parse a size argument (hex with a 0x prefix, or decimal)
pub fn parse_size(s: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|e| format!("无效的十六进制 size '{}': {}", s, e).into())
    } else {
        s.parse::<u32>()
            .map_err(|e| format!("无效的 size '{}': {}", s, e).into())
    }
}
