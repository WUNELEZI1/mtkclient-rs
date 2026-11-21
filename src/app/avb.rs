/// `mtkclient-rs avb <info|disable> <file> [out]`
///
/// Offline mode of the in-house avbtool:
/// - `avb info <vbmeta.img>`: output in the same format as `avbtool info-image`;
/// - `avb disable <vbmeta.img> [out]`: write out with flags=3 (hashtree+verification disabled)
///   (overwrites the original file by default; writes `<file>.disabled` when out is omitted).
pub(crate) fn cmd_avb_offline(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use crate::color::Colorize;
    use crate::security::avb::{FLAG_HASHTREE_DISABLED, FLAG_VERIFICATION_DISABLED, VbmetaImage};

    let usage = "用法: mtkclient-rs avb <info|disable> <vbmeta.img> [out]";
    let action = args.first().ok_or(usage)?;
    let file = args.get(1).ok_or(usage)?;
    let data = std::fs::read(file).map_err(|e| format!("读取 {file} 失败: {e}"))?;
    let vb = VbmetaImage::parse(&data).map_err(|e| format!("解析 vbmeta 失败: {e}"))?;

    match action.as_str() {
        "info" => {
            print!("{}", vb.info());
            Ok(())
        }
        "disable" => {
            let out_path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| format!("{file}.disabled"));
            let flags = FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED;
            let patched = vb.disable_verification(flags);
            std::fs::write(&out_path, &patched)
                .map_err(|e| format!("写出 {out_path} 失败: {e}"))?;
            println!(
                "{}",
                format!("vbmeta 已禁用校验 (flags=0x{flags:02X}) -> {out_path}").green()
            );
            Ok(())
        }
        _ => Err(usage.into()),
    }
}
