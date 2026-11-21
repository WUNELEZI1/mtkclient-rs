//! Shared helpers for reading the resume state file (`.resume`).
//!
//! Path assembly for `<output>.resume`, the `active_read=true` check and `u64` field parsing used to be
//! scattered across `da/xflash/io.rs`, `cmd/mod.rs`, `partition/io.rs`, `cmd/partition_table/mod.rs`
//! and others; they are consolidated here to avoid duplicate implementations and behavior drift.

/// Build the read resume state file path: `<output>.resume`
pub fn read_resume_path(output: &str) -> String {
    format!("{}.resume", output)
}

/// Whether the resume state means "an active read is unfinished" (i.e. contains an `active_read=true` line)
pub fn is_active_read(content: &str) -> bool {
    content.lines().any(|line| line == "active_read=true")
}

/// Parse a `prefix=value` u64 field from the resume state; `None` if the prefix is missing or the value does not parse
pub fn parse_u64_field(content: &str, prefix: &str) -> Option<u64> {
    content
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|value| value.parse::<u64>().ok())
}
