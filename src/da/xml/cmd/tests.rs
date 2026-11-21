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
