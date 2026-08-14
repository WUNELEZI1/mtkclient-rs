//! 读取续传状态文件(`.resume`)的共享辅助。
//!
//! `<output>.resume` 的路径拼装、`active_read=true` 判定、`u64` 字段解析原本散落于
//! `da/xflash/io.rs`、`cmd/mod.rs`、`partition/io.rs`、`cmd/partition_table/mod.rs` 等多处，
//! 统一收敛到此模块，避免重复实现与行为漂移。

/// 拼接读取续传状态文件路径：`<output>.resume`
pub fn read_resume_path(output: &str) -> String {
    format!("{}.resume", output)
}

/// 判定续传状态内容是否为"活跃读取未完成"（即含 `active_read=true` 行）
pub fn is_active_read(content: &str) -> bool {
    content.lines().any(|line| line == "active_read=true")
}

/// 从续传状态内容解析形如 `prefix=value` 的 u64 字段；前缀缺失或数值解析失败返回 `None`
pub fn parse_u64_field(content: &str, prefix: &str) -> Option<u64> {
    content
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|value| value.parse::<u64>().ok())
}
