//! Low-level DAXFlash write primitives
//!
//! Submodules:
//! - `stream` — stream data to a raw address (progress bar / resume)
//! - `cmd`    — write packet length negotiation (SET_PKT_LEN) and write command dispatch
//!
//! Write resume helpers and status error explanation (free functions called by path):
//! - `write_resume_path_for`       — build the `.wresume` file path
//! - `write_write_resume_file`     — write the resume state file
//! - `remove_write_resume_file`    — delete the resume state file
//! - `check_write_resume`          — check whether resume state matches
//! - `explain_write_status` / `format_write_status_error` — explain write status errors

use crate::da::xflash::protocol::CMD_WRITE_DATA;

pub(crate) mod cmd;
pub(crate) mod stream;

// ═══════════════════════════════════════════════════════════════════════════════
// Write resume helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Build the write resume state file path
pub(crate) fn write_resume_path_for(input_file: &str) -> String {
    format!("{}.wresume", input_file)
}

/// Write the resume state file
pub(crate) fn write_write_resume_file(
    input_file: &str,
    addr: u64,
    total: u64,
    written: u64,
    packet_len: Option<usize>,
) -> Result<(), String> {
    let packet = packet_len.unwrap_or(0);
    let content = format!(
        "input={}\naddr=0x{:X}\ntotal={}\nwritten={}\npacket_len={}\n",
        input_file, addr, total, written, packet
    );
    std::fs::write(write_resume_path_for(input_file), content)
        .map_err(|e| format!("写入续传状态失败: {}", e))
}

/// Delete the write resume state file
pub(crate) fn remove_write_resume_file(input_file: &str) {
    let _ = std::fs::remove_file(write_resume_path_for(input_file));
}

/// Check whether write resume state matches
/// Return the number of bytes already written (if matching and valid), else None
pub(crate) fn check_write_resume(input_file: &str, addr: u64, total: u64) -> Option<u64> {
    let content = match std::fs::read_to_string(write_resume_path_for(input_file)) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let resume_addr = content
        .lines()
        .find_map(|line| line.strip_prefix("addr="))
        .and_then(|v| crate::util::parse_u64_hex(v).ok());
    let resume_total = content
        .lines()
        .find_map(|line| line.strip_prefix("total="))
        .and_then(|v| v.parse::<u64>().ok());
    let written = content
        .lines()
        .find_map(|line| line.strip_prefix("written="))
        .and_then(|v| v.parse::<u64>().ok());

    match (resume_addr, resume_total, written) {
        (Some(ra), Some(rt), Some(w)) if ra == addr && rt == total && w > 0 && w < total => Some(w),
        _ => None,
    }
}

pub(crate) fn explain_write_status(status: u32) -> &'static str {
    match status {
        CMD_WRITE_DATA => {
            "读到了 WRITE_DATA 命令 echo，说明 XFlash 状态流错位；请先重试一次，若仍失败请重新进 BROM"
        }
        _ => "DA 返回非零状态",
    }
}

pub(crate) fn format_write_status_error(stage: &str, status: u32) -> String {
    // Classify by structured status code to give a clearer error domain (security/device/protocol…),
    // so users can tell unsupported commands from device-side rejection.
    match crate::error::XFlashError::from_status(status) {
        Some(err) => format!(
            "{} status error: 0x{:08X} [{}] ({})",
            stage,
            err.code,
            err.kind,
            explain_write_status(status)
        ),
        None => format!(
            "{} status error: 0x{:08X} ({})",
            stage,
            status,
            explain_write_status(status)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::da::xflash::protocol::CMD_WRITE_DATA;

    #[test]
    fn write_status_explains_command_echo_desync() {
        assert!(explain_write_status(CMD_WRITE_DATA).contains("WRITE_DATA 命令 echo"));
        assert_eq!(explain_write_status(0xDEAD), "DA 返回非零状态");
    }

    #[test]
    fn write_status_error_includes_stage_code_and_explanation() {
        let message = format_write_status_error("cmd_write_data", CMD_WRITE_DATA);

        assert!(message.contains("cmd_write_data status error"));
        assert!(message.contains("0x00010004"));
        assert!(message.contains("状态流错位"));
    }

    #[test]
    fn write_status_error_classifies_device_domain() {
        // 0xC004____ → device-domain error; the message should carry the category label
        let message = format_write_status_error("writeflash final", 0xC004_0040);
        assert!(message.contains("0xC0040040"), "{}", message);
        assert!(message.contains("设备域错误"), "{}", message);
    }
}
