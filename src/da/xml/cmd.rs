//! XML (V6) DA 命令构造
//!
//! 对齐 penumbra `core/src/da/xml/cmd.rs`：把每条命令序列化为 DA 可解析的
//! XML 报文。报文格式为单行、以 `\0` 结尾：
//!
//! ```text
//! <?xml version="1.0" encoding="utf-8"?><da><version>1.0</version>
//! <command>CMD:SET-BOOT-MODE</command><arg>...</arg></da>\0
//! ```
//!
//! 约定：
//! - 命令名由结构体名的 SCREAMING-KEBAB-CASE 生成（如 `SetBootMode` →
//!   `SET-BOOT-MODE`），与 penumbra 的 `XmlCommand` derive 宏一致；
//! - 缺省参数段为 `<arg>`，具名参数段沿用段名（如 `adv`）；
//! - 标签路径中的 `/` 表示嵌套（如 `arg/message` → `<arg><message>`）。
//!
//! 本模块为纯函数（无 I/O），便于单测覆盖。

#![allow(dead_code)] // V6 命令全集：CLI 目前仅接入 reboot 子集，其余待 v6 设备验证后启用

/// 命令生命周期握手的起始标记（设备在每条命令前回发）
pub const CMD_START: &[u8] = b"<command>CMD:START</command>";
/// 命令生命周期握手的结束标记（设备在每条命令后回发）
pub const CMD_END: &[u8] = b"<command>CMD:END</command>";

/// 设备 → 主机：下载文件（主机需发送数据给设备）
pub const CMD_DOWNLOAD_FILE: &str = "CMD:DOWNLOAD-FILE";
/// 设备 → 主机：上传文件（设备需发送数据给主机）
pub const CMD_UPLOAD_FILE: &str = "CMD:UPLOAD-FILE";
/// 设备 → 主机：进度上报
pub const CMD_PROGRESS_REPORT: &str = "CMD:PROGRESS-REPORT";
/// 设备 → 主机：文件系统操作（创建目录/查询存在性等）
pub const CMD_FILE_SYSTEM_OP: &str = "CMD:FILE-SYS-OPERATION";

/// XML 命令生命周期
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlCmdLifetime {
    /// 命令开始（`CMD:START`）
    CmdStart,
    /// 命令结束（`CMD:END`）
    CmdEnd,
}

impl XmlCmdLifetime {
    /// 对应的标记字节
    pub fn pattern(self) -> &'static [u8] {
        match self {
            XmlCmdLifetime::CmdStart => CMD_START,
            XmlCmdLifetime::CmdEnd => CMD_END,
        }
    }
}

/// (伪) 文件系统操作
///
/// DA 侧通过 `CMD:FILE-SYS-OPERATION` 询问主机要执行的文件操作，
/// 主机以 `OK@<op>\0` 回应。SPFT 用它做目录创建/存在性判断；
/// 这里只需要让协议流程继续，故 `Exists` 直接返回 `NOT-EXISTS`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystemOp {
    /// 创建目录
    MkDir,
    /// 查询是否存在（为避免额外读，直接返回不存在）
    Exists,
    /// 告知文件大小（十六进制）
    FileSize(usize),
    /// 删除整个目录
    RemoveAll,
    /// 删除文件
    Remove,
}

impl FileSystemOp {
    /// 序列化为 DA 期望的应答文本（不含 `OK@` 前缀与结尾 `\0`）
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
// 通用序列化
// =============================================================================

/// 把一个（可能嵌套的）标签路径写入报文体
///
/// `tag_path` 形如 `message` 或 `arg/message`，按 `/` 逐层展开为嵌套标签。
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

/// 通用命令构造器
///
/// `args` 中每项为 `(section, tag_path, content)`：
/// - `section = None` → 归入缺省 `<arg>` 段；
/// - `section = Some(name)` → 归入 `<name>` 段。
///
/// 段顺序：缺省段在前，具名段按首次出现顺序排列（对齐 penumbra 的
/// `BTreeMap<Option<&str>, _>`：`None` 小于 `Some`）。
pub fn create_cmd(name: &str, version: &str, args: &[(Option<&str>, &str, String)]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><da><version>{}</version><command>CMD:{}</command>",
        version, name
    );

    // 缺省段
    if args.iter().any(|(s, _, _)| s.is_none()) {
        xml.push_str("<arg>");
        for (section, tag, content) in args.iter().filter(|(s, _, _)| s.is_none()) {
            let _ = section;
            push_tag(&mut xml, tag, content);
        }
        xml.push_str("</arg>");
    }

    // 具名段（按首次出现顺序）
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
// DA 加载 / 初始化命令
// =============================================================================

/// `SET-RUNTIME-PARAMETER`：设置 DA 日志级别、日志通道与主机系统
///
/// 参考 penumbra `SetRuntimeParameter`（version 1.1），`initialize_dram`
/// 位于独立的 `adv` 段。
pub fn set_runtime_parameter(log_level: &str, log_channel: &str, system_os: &str) -> String {
    create_cmd(
        "SET-RUNTIME-PARAMETER",
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

/// `HOST-SUPPORTED-COMMANDS`：告知设备主机支持的命令能力集
pub fn host_supported_commands() -> String {
    create_cmd(
        "HOST-SUPPORTED-COMMANDS",
        "1.0",
        &[(
            None,
            "host_capability",
            "CMD:DOWNLOAD-FILE^1@CMD:FILE-SYS-OPERATION^1@CMD:PROGRESS-REPORT^1@CMD:UPLOAD-FILE^1@"
                .to_string(),
        )],
    )
}

/// `SET-HOST-INFO`：上报主机工具标识
pub fn set_host_info(info: &str) -> String {
    create_cmd("SET-HOST-INFO", "1.0", &[(None, "info", info.to_string())])
}

/// `NOTIFY-INIT-HW`：通知设备初始化 DRAM
pub fn notify_init_hw() -> String {
    create_cmd("NOTIFY-INIT-HW", "1.0", &[])
}

/// `BOOT-TO`：跳转到指定地址执行，数据源为 `MEM://0x0:0x0`
pub fn boot_to(at_addr: u64, jmp_addr: u64) -> String {
    create_cmd(
        "BOOT-TO",
        "1.0",
        &[
            (None, "at_address", format!("0x{:x}", at_addr)),
            (None, "jmp_address", format!("0x{:x}", jmp_addr)),
            (None, "source_file", "MEM://0x0:0x0".to_string()),
        ],
    )
}

// =============================================================================
// 信息查询命令
// =============================================================================

/// `GET-SYS-PROPERTY`：读取设备系统属性
pub fn get_sys_property(key: &str) -> String {
    create_cmd(
        "GET-SYS-PROPERTY",
        "1.0",
        &[
            (None, "key", key.to_string()),
            (None, "target_file", "MEM://0x0:0x200000".to_string()),
        ],
    )
}

/// `GET-HW-INFO`：读取存储/硬件信息（结果经 UPLOAD-FILE 回传）
pub fn get_hw_info() -> String {
    create_cmd(
        "GET-HW-INFO",
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

// =============================================================================
// 分区/闪存命令
// =============================================================================

/// `READ-PARTITION`：按分区名读取
pub fn read_partition(partition: &str) -> String {
    create_cmd(
        "READ-PARTITION",
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "target_file", format!("{}.bin", partition)),
        ],
    )
}

/// `READ-FLASH`：按地址/长度读取
pub fn read_flash(partition: &str, length: usize, offset: u64) -> String {
    create_cmd(
        "READ-FLASH",
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "target_file", partition.to_string()),
            (None, "length", format!("0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `WRITE-PARTITION`：按分区名写入
pub fn write_partition(partition: &str) -> String {
    create_cmd(
        "WRITE-PARTITION",
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "source_file", format!("{}.bin", partition)),
        ],
    )
}

/// `WRITE-FLASH`：按地址/长度写入
pub fn write_flash(partition: &str, length: usize, offset: u64) -> String {
    create_cmd(
        "WRITE-FLASH",
        "1.0",
        &[
            (None, "partition", partition.to_string()),
            (None, "source_file", format!("MEM:\\0x0:0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `ERASE-PARTITION`：擦除整个分区
pub fn erase_partition(partition: &str) -> String {
    create_cmd(
        "ERASE-PARTITION",
        "1.0",
        &[(None, "partition", partition.to_string())],
    )
}

/// `ERASE-FLASH`：按地址/长度擦除
pub fn erase_flash(section: &str, length: usize, offset: u64) -> String {
    create_cmd(
        "ERASE-FLASH",
        "1.0",
        &[
            (None, "partition", section.to_string()),
            (None, "length", format!("0x{:X}", length)),
            (None, "offset", format!("0x{:X}", offset)),
        ],
    )
}

/// `FLASH-UPDATE`：进入 scatter 刷写流程
pub fn flash_update() -> String {
    let sep = if cfg!(windows) { "\\" } else { "/" };
    create_cmd(
        "FLASH-UPDATE",
        "1.0",
        &[
            (None, "source_file", "./scatter.xml".to_string()),
            (None, "path_separator", sep.to_string()),
            (None, "backup_folder", ".".to_string()),
        ],
    )
}

// =============================================================================
// 重启 / BootMode
// =============================================================================

/// `REBOOT`：立即重启或断开重启
pub fn reboot(disconnect: bool) -> String {
    let action = if disconnect {
        "DISCONNECT"
    } else {
        "IMMEDIATE"
    };
    create_cmd("REBOOT", "1.0", &[(None, "action", action.to_string())])
}

/// `SET-BOOT-MODE`：设置下次启动模式（FASTBOOT / META 等）
pub fn set_boot_mode(mode: &str, connect_type: &str, mobile_log: &str, adb: &str) -> String {
    create_cmd(
        "SET-BOOT-MODE",
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
// Efuse / 安全
// =============================================================================

/// `READ-EFUSE`
pub fn read_efuse() -> String {
    create_cmd(
        "READ-EFUSE",
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `WRITE-EFUSE`
pub fn write_efuse() -> String {
    create_cmd(
        "WRITE-EFUSE",
        "1.0",
        &[(None, "source_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `SECURITY-GET-DEV-FW-INFO`
pub fn security_get_dev_fw_info() -> String {
    create_cmd(
        "SECURITY-GET-DEV-FW-INFO",
        "1.0",
        &[(None, "target_file", "MEM://0x0:0x200000".to_string())],
    )
}

/// `SECURITY-SET-FLASH-POLICY`
pub fn security_set_flash_policy(source_file: &str) -> String {
    create_cmd(
        "SECURITY-SET-FLASH-POLICY",
        "1.0",
        &[(None, "source_file", source_file.to_string())],
    )
}

/// `SECURITY-SET-ALLINONE-SIGNATURE`
pub fn security_set_allinone_signature(source_file: &str) -> String {
    create_cmd(
        "SECURITY-SET-ALLINONE-SIGNATURE",
        "1.0",
        &[(None, "source_file", source_file.to_string())],
    )
}

// =============================================================================
// 单元测试
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
        // 缺省段按给定顺序排列
        assert!(xml.contains(
            "<arg><mode>FASTBOOT</mode><connect_type>USB</connect_type>\
             <mobile_log>ON</mobile_log><adb>ON</adb></arg>"
        ));
        assert!(xml.ends_with("</da>\u{0}"));
    }

    #[test]
    fn runtime_parameter_has_named_adv_section() {
        let xml = set_runtime_parameter("INFO", "USB", "WINDOWS");
        // 缺省段在前，adv 段在后
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
        // 未知文本回退为 Exists
        assert_eq!(FileSystemOp::from("what"), FileSystemOp::Exists);

        assert_eq!(FileSystemOp::MkDir.default_value(), "MKDIR\u{0}");
        assert_eq!(FileSystemOp::FileSize(0xABC).default_value(), "0xABC\u{0}");
    }

    #[test]
    fn lifetime_patterns() {
        assert_eq!(XmlCmdLifetime::CmdStart.pattern(), CMD_START);
        assert_eq!(XmlCmdLifetime::CmdEnd.pattern(), CMD_END);
    }
}
