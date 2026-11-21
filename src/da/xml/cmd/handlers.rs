use crate::obf;

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
