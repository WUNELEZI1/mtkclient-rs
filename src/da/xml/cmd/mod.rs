//! XML (V6) DA command construction
//!
//! Mirrors penumbra `core/src/da/xml/cmd.rs`: serializes each command into a
//! XML message the DA can parse. The message is a single line terminated by `\0`:
//!
//! ```text
//! <?xml version="1.0" encoding="utf-8"?><da><version>1.0</version>
//! <command>CMD:SET-BOOT-MODE</command><arg>...</arg></da>\0
//! ```
//!
//! Conventions:
//! - Command names derive from struct names in SCREAMING-KEBAB-CASE (e.g. `SetBootMode` →
//!   `SET-BOOT-MODE`), matching penumbra's `XmlCommand` derive macro;
//! - The default argument section is `<arg>`; named sections keep their name (e.g. `adv`);
//! - A `/` in a tag path denotes nesting (e.g. `arg/message` → `<arg><message>`).
//!
//! Pure functions only (no I/O), which keeps the module easy to unit test.

use crate::obf;

/// Start marker of the command lifecycle handshake (echoed by the device before each command).
///
/// The plaintext is obfuscated at compile time by `obf!` so the V6/XML protocol fingerprint is not exposed in plaintext.
pub fn cmd_start() -> Vec<u8> {
    obf!("<command>CMD:START</command>").into_bytes()
}

/// End marker of the command lifecycle handshake (echoed after each command). See [`cmd_start`].
pub fn cmd_end() -> Vec<u8> {
    obf!("<command>CMD:END</command>").into_bytes()
}

/// Device → host: download a file (the host must send data to the device)
pub const CMD_DOWNLOAD_FILE: &str = "CMD:DOWNLOAD-FILE";
/// Device → host: upload a file (the device sends data to the host)
pub const CMD_UPLOAD_FILE: &str = "CMD:UPLOAD-FILE";
/// Device → host: progress report
pub const CMD_PROGRESS_REPORT: &str = "CMD:PROGRESS-REPORT";
/// Device → host: file system operation (mkdir, existence check, etc.)
pub const CMD_FILE_SYSTEM_OP: &str = "CMD:FILE-SYS-OPERATION";

/// XML command lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlCmdLifetime {
    /// Command start (`CMD:START`)
    CmdStart,
    /// Command end (`CMD:END`)
    CmdEnd,
}

impl XmlCmdLifetime {
    /// Corresponding marker bytes (restored at runtime by `obf!`)
    pub fn pattern(self) -> Vec<u8> {
        match self {
            XmlCmdLifetime::CmdStart => cmd_start(),
            XmlCmdLifetime::CmdEnd => cmd_end(),
        }
    }
}

/// (Pseudo) file system operation
///
/// The DA asks the host which file operation to run via `CMD:FILE-SYS-OPERATION`,
/// and the host replies with `OK@<op>\0`. SPFT uses this for mkdir and existence checks;
/// here we only need the protocol flow to continue, so `Exists` returns `NOT-EXISTS` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystemOp {
    /// Create a directory
    MkDir,
    /// Query existence (returns not-exists directly to avoid an extra read)
    Exists,
    /// Report the file size (hex)
    FileSize(usize),
    /// Remove the whole directory
    RemoveAll,
    /// Remove a file
    Remove,
}

impl FileSystemOp {
    /// Serialize into the reply text expected by the DA (without the `OK@` prefix and trailing `\0`)
    pub fn default_value(&self) -> String {
        match self {
            FileSystemOp::MkDir => "MKDIR\u{0}".to_string(),
            FileSystemOp::Exists => "NOT-EXISTS\u{0}".to_string(),
            FileSystemOp::FileSize(size) => format!("0x{:X}\u{0}", size),
            FileSystemOp::RemoveAll => "REMOVE-ALL\u{0}".to_string(),
            FileSystemOp::Remove => "REMOVE\u{0}".to_string(),
        }
    }
}

impl From<FileSystemOp> for String {
    fn from(op: FileSystemOp) -> Self {
        op.default_value()
    }
}

impl From<&str> for FileSystemOp {
    fn from(s: &str) -> Self {
        match s {
            "MKDIR" => FileSystemOp::MkDir,
            "NOT-EXISTS" => FileSystemOp::Exists,
            "REMOVE-ALL" => FileSystemOp::RemoveAll,
            "REMOVE" => FileSystemOp::Remove,
            hex => {
                usize::from_str_radix(hex, 16).map_or(FileSystemOp::Exists, FileSystemOp::FileSize)
            }
        }
    }
}

mod handlers;

// =============================================================================
// Public re-exports (keeps crate::da::xml::cmd::* reachable)
// =============================================================================

pub use handlers::*;

#[cfg(test)]
mod tests;
