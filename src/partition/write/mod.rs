//! DAXFlash 底层写入原语
//!
//! 子模块划分：
//! - `stream` — 按原始地址流式写入数据（带进度条/续传）
//! - `cmd`    — 写包长度协商（SET_PKT_LEN）与写命令发送
//!
//! 写入断点续传辅助函数与状态错误解释（自由函数，外部按路径调用）：
//! - `write_resume_path_for`       — 生成 `.wresume` 文件路径
//! - `write_write_resume_file`     — 写入续传状态文件
//! - `remove_write_resume_file`    — 删除续传状态文件
//! - `check_write_resume`          — 检查续传状态是否匹配
//! - `explain_write_status` / `format_write_status_error` — 写状态错误解释

use crate::da::xflash::protocol::CMD_WRITE_DATA;

pub(crate) mod cmd;
pub(crate) mod stream;

// ═══════════════════════════════════════════════════════════════════════════════
// 写入断点续传辅助函数
// ═══════════════════════════════════════════════════════════════════════════════

/// 生成写入续传状态文件路径
pub(crate) fn write_resume_path_for(input_file: &str) -> String {
    format!("{}.wresume", input_file)
}

/// 写入续传状态文件
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

/// 删除写入续传状态文件
pub(crate) fn remove_write_resume_file(input_file: &str) {
    let _ = std::fs::remove_file(write_resume_path_for(input_file));
}

/// 检查写入续传状态是否匹配
/// 返回已写入字节数（若匹配且有效），否则 None
pub(crate) fn check_write_resume(input_file: &str, addr: u64, total: u64) -> Option<u64> {
    let content = match std::fs::read_to_string(write_resume_path_for(input_file)) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let resume_addr = content
        .lines()
        .find_map(|line| line.strip_prefix("addr=0x"))
        .and_then(|v| u64::from_str_radix(v, 16).ok());
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
    format!(
        "{} status error: 0x{:08X} ({})",
        stage,
        status,
        explain_write_status(status)
    )
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
}
