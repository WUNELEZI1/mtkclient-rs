//! XML DA 命令协议（Layer 3，新平台 MT6789+）

use super::XmlBootMode;

impl XmlBootMode {
    fn as_str(&self) -> &'static str {
        match self {
            XmlBootMode::Fastboot => "FASTBOOT",
            XmlBootMode::Meta => "META",
        }
    }
}

/// 构建 XML DA 命令包
///
/// XML 协议格式（新平台 MT6789+ 使用）：
/// ```xml
/// <?xml version="1.0" encoding="utf-8"?>
/// <da>
///   <version>1.0</version>
///   <command>CMD:{命令名}</command>
///   <arg>{参数XML}</arg>
/// </da>
/// ```
fn build_xml_command(cmd_name: &str, arg_content: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
         <da>\r\n\
         <version>1.0</version>\r\n\
         <command>CMD:{}</command>\r\n\
         <arg>\r\n\
         {}\r\n\
         </arg>\r\n\
         </da>",
        cmd_name, arg_content
    )
    .into_bytes()
}

/// 构建 SET-BOOT-MODE XML 命令
///
/// 对齐 mtkclient da_cmd.py cmd_set_boot_mode：
/// ```xml
/// <mode>FASTBOOT</mode>
/// <connect_type>USB</connect_type>
/// <mobile_log>ON</mobile_log>
/// <adb>ON</adb>
/// ```
pub fn xml_set_boot_mode(mode: XmlBootMode) -> Vec<u8> {
    let arg = format!(
        "<mode>{}</mode>\r\n\
         <connect_type>USB</connect_type>\r\n\
         <mobile_log>ON</mobile_log>\r\n\
         <adb>ON</adb>",
        mode.as_str()
    );
    build_xml_command("SET-BOOT-MODE", &arg)
}

/// 构建 REBOOT XML 命令
///
/// 对齐 mtkclient da_cmd.py cmd_reboot：
/// ```xml
/// <action>IMMEDIATE</action>
/// ```
pub fn xml_reboot(disconnect: bool) -> Vec<u8> {
    let action = if disconnect {
        "DISCONNECT"
    } else {
        "IMMEDIATE"
    };
    let arg = format!("<action>{}</action>", action);
    build_xml_command("REBOOT", &arg)
}
