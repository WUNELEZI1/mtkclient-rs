//! Partition read resume offset calculation

/// Compute the resume start offset for a partition read (shared by the single-partition read and `rl` batch reads).
///
/// Rules:
/// - Output file missing or empty → return `0` (read from the start).
/// - File already complete (existing size >= target size) → return the target size so the caller marks it done and skips.
/// - File partially present → **truncate to a 512-byte aligned boundary** (avoiding a misaligned resume) and return that offset as the resume point.
///
/// ⚠️ Side-effect warning: this is not a pure query. For a partially present file it **modifies the disk**
/// (`set_len` truncation to the 512-byte boundary), even if the caller only wanted to check whether a resume is needed.
/// Confirm the truncation is acceptable before calling; the resume logic relies on this alignment to avoid overlap and misalignment.
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
        return size; // the caller skips based on this
    }
    // Partially present: truncate to a 512-byte boundary and resume from there
    let aligned = (existing / 512) * 512;
    if aligned != existing {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(output) {
            let _ = file.set_len(aligned);
        }
    }
    aligned
}
