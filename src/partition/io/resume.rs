//! 分区读取断点续传偏移计算

/// 计算分区读取的断点续传起始偏移（供单分区 `读取分区` 与 `rl` 批量读取共用）。
///
/// 规则：
/// - 输出文件不存在或为空 → 返回 `0`（从头读取）。
/// - 文件已完整（现有大小 >= 目标大小）→ 返回 `目标大小`，调用方据此判定“已完成”并跳过。
/// - 文件部分存在 → **截断到 512 字节对齐边界**（避免续读错位），并返回该对齐偏移作为续传起点。
///
/// ⚠️ 副作用警告：本函数并非纯查询。对“部分存在”的文件，它会**直接修改磁盘**
/// （`set_len` 截断到 512 对齐点），即使调用方只是想“判断是否需要续传”。
/// 调用前请确认已接受该文件可能被截断；续读逻辑依赖此对齐保证数据不重叠、不错位。
pub(crate) fn compute_read_resume_offset(output: &str, size: u64) -> u64 {
    let path = std::path::Path::new(output);
    let existing = if path.exists() {
        std::fs::metadata(output).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if existing == 0 {
        return 0;
    }
    if existing >= size {
        return size; // 调用方据此跳过
    }
    // 部分存在：截断到 512 字节对齐边界，从对齐点续读
    let aligned = (existing / 512) * 512;
    if aligned != existing {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(output) {
            let _ = file.set_len(aligned);
        }
    }
    aligned
}
