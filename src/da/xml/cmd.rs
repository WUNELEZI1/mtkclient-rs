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

// =============================================================================
// Common serialization
// =============================================================================

/// Write a (possibly nested) tag path into the message body
///
/// `tag_path` is either `message` or `arg/message`; each `/` level expands into nested tags.
fn push_tag(xml: &mut String, tag_path: &str, content: &str) {
    let parts: Vec<&str> = tag_path.split('/').collect();
    for p in &parts {
        xml.push('<');
        xml.push_str(p);
        xml.push('>');
    }
    xml.push_str(content);
    for p in parts.iter().rev() {
        xml.push_str("</");
        xml.push_str(p);
        xml.push('>');
    }
}

/// Generic command builder
///
/// Each entry in `args` is `(section, tag_path, content)`:
/// - `section = None` → placed in the default `<arg>` section;
/// - `section = Some(name)` → placed in the `<name>` section.
///
/// Section order: the default section first, then named sections in first-appearance order
/// (matching penumbra's `BTreeMap<Option<&str>, _>`, where `None` sorts before `Some`).
pub fn create_cmd(name: &str, version: &str, args: &[(Option<&str>, &str, String)]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><da><version>{}</version><command>CMD:{}</command>",
        version, name
    );

    // Default section
    if args.iter().any(|(s, _, _)| s.is_none()) {
        xml.push_str("<arg>");
        for (section, tag, content) in args.iter().filter(|(s, _, _)| s.is_none()) {
            let _ = section;
            push_tag(&mut xml, tag, content);
        }
        xml.push_str("</arg>");
    }

    // Named sections (in first-appearance order)
    let mut seen: Vec<&str> = Vec::new();
    for (section, _, _) in args {
        if let Some(name) = section {
            if !seen.contains(name) {
                seen.push(name);
            }
        }
    }
    for name in seen {
        xml.push('<');
        xml.push_str(name);
        xml.push('>');
        for (_, tag, content) in args.iter().filter(|(s, _, _)| *s == Some(name)) {
            push_tag(&mut xml, tag, content);
        }
        xml.push_str("</");
        xml.push_str(name);
        xml.push('>');
    }

    xml.push_str("</da>\u{0}");
    xml
}

// =============================================================================
// DA load / initialization commands
// =============================================================================

/// `SET-RUNTIME-PARAMETER`: set the DA log level, log channel, and host OS
///
/// Mirrors penumbra `SetRuntimeParameter` (version 1.1); `initialize_dram`
/// lives in a separate `adv` section.
pub fn set_runtime_parameter(log_level: &str, log_channel: &str, system_os: &str) -> String {
    create_cmd(
        &obf!("SET-RUNTIME-PARAMETER"),
        "1.1",
        &[
            (None, "checksum_level", "NONE".to_string()),
            (None, "battery_exist", "AUTO-DETECT".to_string()),
            (None, "da_log_level", log_level.to_string()),
            (None, "log_channel", log_channel.to_string()),
            (None, "system_os", system_os.to_string()),
            (Some("adv"), "initialize_dram", "YES".to_string()),
        ],
    )
}

/// `HOST-SUPPORTED-COMMANDS`: advertise the host's supported command capabilities to the device
pub fn host_supported_commands() -> String {
    create_cmd(
        &obf!("HOST-SUPPORTED-COMMANDS"),
        "1.0",
        &[(
            None,
            "host_capability",
            obf!(
                "CMD:DOWNLOAD-FILE^1@CMD:FILE-SYS-OPERATION^1@CMD:PROGRESS-REPORT^1@CMD:UPLOAD-FILE^1@"
            ),
        )],
    )
}

/// `SET-HOST-INFO`: report the host tool identity
pub fn set_host_info(info: &str) -> String {
    create_cmd(
        &obf!("SET-HOST-INFO"),
        "1.0",
        &[(None, "info", info.to_string())],
    )
}

/// `NOTIFY-INIT-HW`: ask the device to initialize DRAM
pub fn notify_init_hw() -> String {
    create_cmd(&obf!("NOTIFY-INIT-HW"), "1.0", &[])
}

/// `BOOT-TO`: jump to the given address and execute; the data source is `MEM://0x0:0x0`
pub fn boot_to(at_addr: u64, jmp_addr: u64) -> String {
    create_cmd(
        &obf!("BOOT-TO"),
        "1.0",
        &[
            (None, "at_address", format!("0x{:x}", at_addr)),
            (None, "jmp_address", format!("0x{:x}", jmp_addr)),
            (None, "source_file", "MEM://0x0:0x0".to_string()),
        ],
    )
}

// =============================================================================
// Information query commands
// =============================================================================

/// `GET-SYS-PROPERTY`: read a device system property
pub fn get_sys_property(key: &str) -> String {
    create_cmd(
        &obf!("GET-SYS-PROPERTY"),
        "1.0",
        &[
            (None, "key", key.to_string()),
            (None, "target_file", "MEM://0x0:0x200000".to_string()),
        ],
    )
}

/// `GET-HW-INFO`: read storage/hardware information (returned via UPLOAD-FILE)
pub fn get_hw_info() -> String {
    create_cmd(
        &obf!("GET-HW-INFO"),
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

// =============================================================================
// Partition / flash commands
// =============================================================================

/// `READ-PARTITION`: read by partition name
pub fn read_partition(partition: &str) -> String {
    create_cmd(
        &obf!("READ-PARTITION"),
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "target_file", format!("{}.bin", partition)),
        ],
    )
}

/// `READ-FLASH`: read by address/length
pub fn read_flash(partition: &str, length: usize, offset: u64) -> String {
    create_cmd(
        &obf!("READ-FLASH"),
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "target_file", partition.to_string()),
            (None, "length", format!("0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `WRITE-PARTITION`: write by partition name
pub fn write_partition(partition: &str) -> String {
    create_cmd(
        &obf!("WRITE-PARTITION"),
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "source_file", format!("{}.bin", partition)),
        ],
    )
}

/// `WRITE-FLASH`: write by address/length
pub fn write_flash(partition: &str, length: usize, offset: u64) -> String {
    create_cmd(
        &obf!("WRITE-FLASH"),
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "source_file", format!("MEM:\\0x0:0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `ERASE-PARTITION`: erase an entire partition
pub fn erase_partition(partition: &str) -> String {
    create_cmd(
        &obf!("ERASE-PARTITION"),
        "1.0",
        &[(None, "partition", partition.to_string())],
    )
}

/// `ERASE-FLASH`: erase by address/length
pub fn erase_flash(section: &str, length: usize, offset: u64) -> String {
    create_cmd(
        &obf!("ERASE-FLASH"),
        "1.0",
        &[
            (None, "partition", section.to_string()),
            (None, "length", format!("0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `FLASH-UPDATE`: enter the scatter flashing flow
pub fn flash_update() -> String {
    let sep = if cfg!(windows) { "\\" } else { "/" };
    create_cmd(
        &obf!("FLASH-UPDATE"),
        "1.0",
        &[
            (None, "source_file", "./scatter.xml".to_string()),
            (None, "path_separator", sep.to_string()),
            (None, "backup_folder", ".".to_string()),
        ],
    )
}

// =============================================================================
// Reboot / BootMode
// =============================================================================

/// `REBOOT`: reboot immediately or reboot with disconnect
pub fn reboot(disconnect: bool) -> String {
    let action = if disconnect {
        "DISCONNECT"
    } else {
        "IMMEDIATE"
    };
    create_cmd(
        &obf!("REBOOT"),
        "1.0",
        &[(None, "action", action.to_string())],
    )
}

/// `SET-BOOT-MODE`: set the next boot mode (FASTBOOT / META, etc.)
pub fn set_boot_mode(mode: &str, connect_type: &str, mobile_log: &str, adb: &str) -> String {
    create_cmd(
        &obf!("SET-BOOT-MODE"),
        "1.0",
        &[
            (None, "mode", mode.to_string()),
            (None, "connect_type", connect_type.to_string()),
            (None, "mobile_log", mobile_log.to_string()),
            (None, "adb", adb.to_string()),
        ],
    )
}

// =============================================================================
// Efuse / security
// =============================================================================

/// `READ-EFUSE`
pub fn read_efuse() -> String {
    create_cmd(
        &obf!("READ-EFUSE"),
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `WRITE-EFUSE`
pub fn write_efuse() -> String {
    create_cmd(
        &obf!("WRITE-EFUSE"),
        "1.0",
        &[(None, "source_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `SECURITY-GET-DEV-FW-INFO`
pub fn security_get_dev_fw_info() -> String {
    create_cmd(
        &obf!("SECURITY-GET-DEV-FW-INFO"),
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `SECURITY-SET-FLASH-POLICY`
pub fn security_set_flash_policy(source_file: &str) -> String {
    create_cmd(
        &obf!("SECURITY-SET-FLASH-POLICY"),
        "1.0",
        &[(None, "source_file", source_file.to_string())],
    )
}

/// `SECURITY-SET-ALLINONE-SIGNATURE`
pub fn security_set_allinone_signature(source_file: &str) -> String {
    create_cmd(
        &obf!("SECURITY-SET-ALLINONE-SIGNATURE"),
        "1.0",
        &[(None, "source_file", source_file.to_string())],
    )
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reboot_immediate_vs_disconnect() {
        let imm = reboot(false);
        assert!(imm.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?><da>"));
        assert!(imm.contains("<command>CMD:REBOOT</command>"));
        assert!(imm.contains("<arg><action>IMMEDIATE</action></arg>"));
        assert!(imm.ends_with("</da>\u{0}"));

        let disc = reboot(true);
        assert!(disc.contains("<action>DISCONNECT</action>"));
    }

    #[test]
    fn set_boot_mode_layout() {
        let xml = set_boot_mode("FASTBOOT", "USB", "ON", "ON");
        assert!(xml.contains("<command>CMD:SET-BOOT-MODE</command>"));
        // The default section keeps the given order
        assert!(xml.contains(
            "<arg><mode>FASTBOOT</mode><connect_type>USB</connect_type>\
             <mobile_log>ON</mobile_log><adb>ON</adb></arg>"
        ));
        assert!(xml.ends_with("</da>\u{0}"));
    }

    #[test]
    fn runtime_parameter_has_named_adv_section() {
        let xml = set_runtime_parameter("INFO", "USB", "WINDOWS");
        // Default section first, `adv` section after
        let arg_pos = xml.find("<arg>").unwrap();
        let adv_pos = xml.find("<adv>").unwrap();
        assert!(arg_pos < adv_pos, "arg 段应先于 adv 段: {}", xml);
        assert!(xml.contains("<version>1.1</version>"));
        assert!(xml.contains("<initialize_dram>YES</initialize_dram>"));
        assert!(xml.contains("<da_log_level>INFO</da_log_level>"));
        assert!(xml.contains("<system_os>WINDOWS</system_os>"));
    }

    #[test]
    fn read_flash_uses_hex_length_and_offset() {
        let xml = read_flash("boot", 0x1000, 0x200);
        assert!(xml.contains("<command>CMD:READ-FLASH</command>"));
        assert!(xml.contains("<length>0x1000</length>"));
        assert!(xml.contains("<offset>0x200</offset>"));
        assert!(xml.contains("<partition>boot</partition>"));
    }

    #[test]
    fn nested_tag_path_supported() {
        let xml = create_cmd("X", "1.0", &[(None, "arg/message", "hi".to_string())]);
        assert!(
            xml.contains("<arg><arg><message>hi</message></arg></arg>"),
            "{}",
            xml
        );
    }

    #[test]
    fn empty_args_produce_empty_body() {
        let xml = notify_init_hw();
        assert!(xml.contains("<command>CMD:NOTIFY-INIT-HW</command></da>\u{0}"));
    }

    #[test]
    fn file_system_op_roundtrip() {
        assert_eq!(FileSystemOp::from("MKDIR"), FileSystemOp::MkDir);
        assert_eq!(FileSystemOp::from("REMOVE-ALL"), FileSystemOp::RemoveAll);
        assert_eq!(FileSystemOp::from("REMOVE"), FileSystemOp::Remove);
        assert_eq!(FileSystemOp::from("NOT-EXISTS"), FileSystemOp::Exists);
        assert_eq!(FileSystemOp::from("1F40"), FileSystemOp::FileSize(0x1F40));
        // Unknown text falls back to Exists
        assert_eq!(FileSystemOp::from("what"), FileSystemOp::Exists);

        assert_eq!(FileSystemOp::MkDir.default_value(), "MKDIR\u{0}");
        assert_eq!(FileSystemOp::FileSize(0xABC).default_value(), "0xABC\u{0}");
    }

    #[test]
    fn lifetime_patterns() {
        assert_eq!(XmlCmdLifetime::CmdStart.pattern(), cmd_start());
        assert_eq!(XmlCmdLifetime::CmdEnd.pattern(), cmd_end());
    }
}
