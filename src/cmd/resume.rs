//! DA/USB transport error classification and resume-state checks

/// Determine whether an error is a DA/USB transport failure (device disconnect, timeout, endpoint error, etc.).
///
/// Only transport failures should reset the DA session; business errors such as bad command format, bad
/// arguments, missing files, parse errors and security errors mean "the DA session itself is healthy" and
/// must not reset it (a reset forces a re-handshake/Bypass next time, pure waste).
///
/// Priority (exact branching on `AppError` variants to avoid keyword false positives):
/// 1. `AppError::Usb` — a real USB/DA transport failure → yes
/// 2. `AppError::Parse` / `AppError::Security` — business-level errors, never transport failures
///    → no (never reset even if the message contains words like "timeout", preventing false positives)
/// 3. `AppError::Protocol` / `AppError::Io` → no by default; yes only when the message text clearly
///    indicates a transport failure (keywords such as timeout/disconnect/endpoint error, case-insensitive)
/// 4. Non-`AppError` (third-party/std errors) → yes only on a keyword hit.
///
/// Note: after wrapping in `AppError`, `Display` prepends prefixes such as "protocol error:" or
/// "USB error:", so keyword matching uses `contains` rather than `starts_with`.
/// Keywords for deciding "is this a DA/USB transport failure" (case-insensitive).
/// Only a fallback for Protocol/Io errors; Usb/Parse/Security use exact branches.
const TRANSPORT_KEYWORDS: &[&str] = &[
    "usb 错误",
    "超时",
    "timeout",
    "断开",
    "disconnect",
    "read err",
    "write err",
    "libusb",
    "端点",
    "endpoint",
    "设备未找到",
    "device not found",
];

pub fn is_transport_error(e: &(dyn std::error::Error + 'static)) -> bool {
    match e.downcast_ref::<crate::error::AppError>() {
        Some(crate::error::AppError::Usb(_)) => true,
        Some(crate::error::AppError::Parse(_)) | Some(crate::error::AppError::Security(_)) => false,
        Some(crate::error::AppError::Protocol(_)) | Some(crate::error::AppError::Io(_)) => {
            let err_str = e.to_string().to_lowercase();
            TRANSPORT_KEYWORDS.iter().any(|kw| err_str.contains(kw))
        }
        None => {
            // Non-AppError (third-party/std error): classify as transport failure only on a keyword hit
            let err_str = e.to_string().to_lowercase();
            TRANSPORT_KEYWORDS.iter().any(|kw| err_str.contains(kw))
        }
    }
}

pub(crate) fn active_read_resume_exists(args: &[String]) -> bool {
    let Some(output) = args.get(1) else {
        return false;
    };
    let path = crate::resume::read_resume_path(output);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    if !crate::resume::is_active_read(&content) {
        return false;
    }
    let Some(written) = crate::resume::parse_u64_field(&content, "written=") else {
        return false;
    };
    match std::fs::metadata(output) {
        Ok(metadata) => {
            let file_size = metadata.len();
            // BufWriter has a 64MB buffer: on Ctrl+C the file may be smaller than the recorded resume size.
            // Allow file_size <= written (bytes flushed to disk <= bytes accepted by the writer thread).
            // Maximum tolerance 128MB (2x BufWriter buffer + safety margin).
            file_size <= written && written.saturating_sub(file_size) < 128 * 1024 * 1024
        }
        Err(_) => false,
    }
}

/// Check whether a write resume-state file exists
pub(crate) fn active_write_resume_exists(args: &[String]) -> bool {
    if args.len() < 2 {
        return false;
    }
    let input_file = &args[1];
    let resume_path = format!("{}.wresume", input_file);
    let Ok(content) = std::fs::read_to_string(&resume_path) else {
        return false;
    };
    let written = crate::resume::parse_u64_field(&content, "written=");
    written.map(|w| w > 0).unwrap_or(false)
}

/// Info about an unfinished read task (used by reboot and other commands that destroy the DA session, for a safety warning)
#[derive(Debug, Clone)]
pub struct PendingRead {
    /// Output file path (from the resume file's output= field, falling back to the name without .resume)
    pub output: String,
    /// Total partition size in bytes
    pub size: u64,
    /// Bytes written so far
    pub written: u64,
}

/// Scan the given directory and return all partition reads that are "active but unfinished".
///
/// Condition: a `<output>.resume` state file exists and contains `active_read=true`.
/// `readflash_to_file` writes this state when the user cancels (Ctrl+C) a resumable read; normal completion
/// deletes it, and a read error writes `active_read=false`. Only `active_read=true` means "resumable but
/// unfinished"; in that case, running reboot or other DA-session-destroying commands loses the read progress.
pub fn pending_read_resume_in_dir(dir: &str) -> Vec<PendingRead> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("resume") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !crate::resume::is_active_read(&content) {
            continue;
        }
        let output = content
            .lines()
            .find_map(|line| line.strip_prefix("output="))
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.with_extension("").to_string_lossy().to_string());
        let size = crate::resume::parse_u64_field(&content, "size=").unwrap_or(0);
        let written = crate::resume::parse_u64_field(&content, "written=").unwrap_or(0);
        result.push(PendingRead {
            output,
            size,
            written,
        });
    }
    result
}

/// Scan the current working directory for unfinished read tasks
pub fn pending_read_resume_cwd() -> Vec<PendingRead> {
    pending_read_resume_in_dir(".")
}
