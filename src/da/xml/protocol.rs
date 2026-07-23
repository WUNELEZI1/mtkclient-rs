//! DA/XML 协议原语
//!
//! Magic: 0xFEEEEEEF
//! 通信格式：XML 文本消息，每个消息前有 4 字节 magic + 4 字节 length

use log::{debug, trace, warn};
use std::time::Duration;

/// XML DA Magic 值（对齐 Python xml_lib.MAGIC）
pub const XML_MAGIC: u32 = 0xFEEEEEEF;

/// XML 命令常量
pub mod cmd {
    pub const CONNECT: &str = "CONNECT";
    pub const UPLOAD_DA: &str = "UPLOAD-DA";
    pub const SETUP_ENV: &str = "SETUP-ENV";
    pub const SETUP_HW_INIT: &str = "SETUP-HW-INIT";
    pub const GET_EMMC_INFO: &str = "GET-EMMC-INFO";
    pub const GET_CHIP_ID: &str = "GET-CHIP-ID";
    pub const READ_FLASH: &str = "READ-FLASH";
    pub const WRITE_FLASH: &str = "WRITE-FLASH";
    pub const ERASE_FLASH: &str = "ERASE-FLASH";
    pub const FORMAT_FLASH: &str = "FORMAT-FLASH";
    pub const DISCONNECT: &str = "DISCONNECT";
}

/// 构建 XML 消息，自动添加 magic + length 前缀
pub fn pack_xml(cmd: &str, params: &[(String, String)]) -> Vec<u8> {
    let mut xml = format!("<{}>", cmd);
    for (k, v) in params {
        xml.push_str(&format!("<{}>{}</{}>", k, escape_xml(v), k));
    }
    xml.push_str(&format!("</{}>", cmd));

    let data = xml.into_bytes();
    let mut result = Vec::with_capacity(8 + data.len());
    result.extend_from_slice(&XML_MAGIC.to_le_bytes());
    result.extend_from_slice(&(data.len() as u32).to_le_bytes());
    result.extend_from_slice(&data);
    result
}

/// 转义 XML 特殊字符
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 发送 XML 命令并读取响应
/// 对齐 Python xml_lib.send_command
pub fn send_xml_cmd(
    preloader: &mut crate::preloader::Preloader,
    cmd: &str,
    params: &[(String, String)],
) -> Result<XmlResponse, String> {
    let packet = pack_xml(cmd, params);
    debug!("[XML] TX: {} ({} params, {} bytes)", cmd, params.len(), packet.len());

    // 发送数据
    preloader.device.write(&packet)?;

    // 读取 magic
    let mut magic_buf = [0u8; 4];
    preloader.device.set_timeout(Duration::from_millis(5000));
    preloader.device.read_exact(&mut magic_buf)?;
    let magic = u32::from_le_bytes(magic_buf);
    if magic != XML_MAGIC {
        return Err(format!(
            "XML magic 不匹配: 0x{:08X} (期望 0x{:08X})",
            magic, XML_MAGIC
        ));
    }

    // 读取长度
    let mut len_buf = [0u8; 4];
    preloader.device.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len == 0 {
        return Err("XML 响应长度为零".into());
    }
    if len > 16 * 1024 * 1024 {
        return Err(format!("XML 响应过大: {} 字节", len));
    }

    // 读取 XML 数据
    let mut xml_data = vec![0u8; len];
    preloader.device.set_timeout(Duration::from_millis(10000));
    preloader.device.read_exact(&mut xml_data)?;

    let xml_str = String::from_utf8_lossy(&xml_data);
    trace!("[XML] RX: {}", xml_str.trim());

    XmlResponse::parse(&xml_str)
}

/// XML 响应解析结果
#[derive(Debug, Clone)]
pub struct XmlResponse {
    pub command: String,
    pub params: Vec<(String, String)>,
    pub is_ok: bool,
}

impl XmlResponse {
    /// 解析 XML 响应字符串
    pub fn parse(xml: &str) -> Result<Self, String> {
        let xml = xml.trim();
        if xml.is_empty() {
            return Err("空 XML 响应".into());
        }

        // 简单解析：提取最外层标签名和内部参数
        // 格式: <COMMAND><PARAM1>value1</PARAM1>...</COMMAND>
        let start = xml.find('<').ok_or("XML 无起始标签")?;
        let after_start = &xml[start + 1..];
        let tag_end = after_start.find(|c: char| c == '>' || c.is_whitespace())
            .ok_or("XML 标签格式错误")?;
        let command = after_start[..tag_end].to_string();

        // 查找结束标签
        let end_tag = format!("</{}>", command);
        let end_pos = xml.rfind(&end_tag).ok_or("XML 无结束标签")?;
        let inner = &xml[start + 1 + tag_end + 1..end_pos];

        // 解析内部参数
        let mut params = Vec::new();
        let mut pos = 0;
        while pos < inner.len() {
            let Some(tag_start) = inner[pos..].find('<') else { break };
            let abs_start = pos + tag_start;
            if inner.as_bytes().get(abs_start + 1) == Some(&b'/') {
                pos = abs_start + 1;
                continue;
            }
            let Some(tag_close) = inner[abs_start..].find('>') else { break };
            let tag_name = inner[abs_start + 1..abs_start + tag_close].to_string();
            let value_start = abs_start + tag_close + 1;
            let end_tag_str = format!("</{}>", tag_name);
            let Some(value_end) = inner[value_start..].find(&end_tag_str) else { break };
            let value = inner[value_start..value_start + value_end].to_string();
            params.push((tag_name, value));
            pos = value_start + value_end + end_tag_str.len();
        }

        let is_ok = params.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("status") && v.eq_ignore_ascii_case("ok")
        });

        Ok(XmlResponse { command, params, is_ok })
    }

    /// 获取参数值
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// 发送简单 ACK（空参数的 STATUS 响应）
pub fn send_xml_ack(preloader: &mut crate::preloader::Preloader) -> Result<(), String> {
    let packet = pack_xml("STATUS", &[("value".to_string(), "ACK".to_string())]);
    preloader.device.write(&packet).map(|_| ())
}

/// 发送原始数据块（用于 DA 上传、WRITE-FLASH 等）
/// 数据格式：magic(4) + length(4) + data，分 64KB 块传输
pub fn send_xml_data_blocks(
    preloader: &mut crate::preloader::Preloader,
    data: &[u8],
) -> Result<(), String> {
    const CHUNK_SIZE: usize = 0x10000; // 64KB 块

    for chunk in data.chunks(CHUNK_SIZE) {
        let mut packet = Vec::with_capacity(8 + chunk.len());
        packet.extend_from_slice(&XML_MAGIC.to_le_bytes());
        packet.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        packet.extend_from_slice(chunk);
        preloader.device.write(&packet)?;
    }
    Ok(())
}

/// 读取原始数据（用于 READ-FLASH 等命令后的数据读取）
pub fn read_xml_data(
    preloader: &mut crate::preloader::Preloader,
    expected_len: usize,
    timeout_ms: u64,
) -> Result<Vec<u8>, String> {
    if expected_len == 0 {
        return Ok(Vec::new());
    }

    // XML 模式下大数据通过连续的 magic+length+data 传输
    let mut result = Vec::with_capacity(expected_len);
    preloader.device.set_timeout(Duration::from_millis(timeout_ms));
    while result.len() < expected_len {
        let mut magic_buf = [0u8; 4];
        preloader.device.read_exact(&mut magic_buf)?;
        let magic = u32::from_le_bytes(magic_buf);
        if magic != XML_MAGIC {
            warn!("[XML] 数据包 magic 不匹配: 0x{:08X}", magic);
            continue;
        }

        let mut len_buf = [0u8; 4];
        preloader.device.read_exact(&mut len_buf)?;
        let chunk_len = u32::from_le_bytes(len_buf) as usize;
        if chunk_len == 0 {
            break;
        }

        let to_read = chunk_len.min(expected_len - result.len());
        let mut chunk = vec![0u8; to_read];
        preloader.device.read_exact(&mut chunk)?;
        result.extend_from_slice(&chunk);

        // 如果有多余数据（chunk_len > to_read），跳过
        if chunk_len > to_read {
            let skip = chunk_len - to_read;
            let mut skip_buf = vec![0u8; skip];
            let _ = preloader.device.read_exact(&mut skip_buf);
        }
    }
    Ok(result)
}
