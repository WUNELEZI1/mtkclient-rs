use crate::color::Colorize;
use log::info;
use crate::da::DAXFlash;

/// 写入文件到分区
pub fn cmd_write(
    da: &mut DAXFlash,
    args: &[String],
    verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        return Err("用法: mtkclient w <part> <file>".into());
    }
    let result = if verify {
        da.write_partition_with_verify(&args[0], &args[1])
    } else {
        da.write_partition(&args[0], &args[1])
    };
    result.map_err(|e| format!("写入失败: {}", e))?;
    info!("{}", format!("{} <- {}", args[0], args[1]).green());
    Ok(())
}
