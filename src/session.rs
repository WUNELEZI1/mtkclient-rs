use log::{debug, info, warn};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// DA 会话状态文件路径
const STATE_FILE: &str = ".state";

/// 会话状态（对齐 Python .state 文件机制）
#[derive(Debug, Clone)]
pub struct SessionState {
    pub usb_vid: u16,
    pub usb_pid: u16,
    pub hw_code: u16,
    pub target_config: u32,
    pub da_loaded: bool,
}

impl SessionState {
    /// 序列化到 key=value 格式
    pub fn to_string(&self) -> String {
        format!(
            "usb_vid=0x{:04X}\nusb_pid=0x{:04X}\nhw_code=0x{:04X}\ntarget_config=0x{:08X}\nda_loaded={}",
            self.usb_vid, self.usb_pid, self.hw_code, self.target_config, self.da_loaded
        )
    }

    /// 从 key=value 格式反序列化
    pub fn from_string(s: &str) -> Option<Self> {
        let mut map = HashMap::new();
        for line in s.lines() {
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim(), v.trim());
            }
        }

        Some(SessionState {
            usb_vid: u16::from_str_radix(map.get("usb_vid")?.strip_prefix("0x")?, 16).ok()?,
            usb_pid: u16::from_str_radix(map.get("usb_pid")?.strip_prefix("0x")?, 16).ok()?,
            hw_code: u16::from_str_radix(map.get("hw_code")?.strip_prefix("0x")?, 16).ok()?,
            target_config: u32::from_str_radix(map.get("target_config")?.strip_prefix("0x")?, 16).ok()?,
            da_loaded: map.get("da_loaded")?.parse::<bool>().ok()?,
        })
    }

    /// 写入 .state 文件
    pub fn save(&self) -> Result<(), String> {
        fs::write(STATE_FILE, self.to_string())
            .map_err(|e| format!("写入 .state 失败: {}", e))
    }

    /// 读取 .state 文件
    pub fn load() -> Option<Self> {
        if !Path::new(STATE_FILE).exists() {
            return None;
        }
        match fs::read_to_string(STATE_FILE) {
            Ok(content) => {
                let state = Self::from_string(&content)?;
                debug!("[session] .state 已加载: hw_code=0x{:04X}, da_loaded={}", state.hw_code, state.da_loaded);
                Some(state)
            }
            Err(e) => {
                warn!("[session] 读取 .state 失败: {}", e);
                None
            }
        }
    }

    /// 删除 .state 文件
    pub fn remove() {
        if Path::new(STATE_FILE).exists() {
            let _ = fs::remove_file(STATE_FILE);
            debug!("[session] .state 已删除");
        }
    }

    /// 检查设备是否在线（VID/PID 匹配）
    pub fn device_online(&self, vid: u16, pid: u16) -> bool {
        self.usb_vid == vid && self.usb_pid == pid
    }
}

/// 尝试复用现有 DA 会话
/// 如果 .state 存在且设备已处于 DA 模式（PID=0x2000），跳过 BROM→DA 流程
pub fn try_reuse_da_session(vid: u16, pid: u16) -> bool {
    if let Some(state) = SessionState::load() {
        if state.da_loaded && state.device_online(vid, pid) {
            info!("[session] 复用 DA 会话（hw_code=0x{:04X}，设备在线）", state.hw_code);
            return true;
        } else {
            debug!("[session] .state 存在但设备不在线或 DA 未加载，重新初始化");
        }
    }
    false
}

/// 保存 DA 会话状态（在 DA 加载成功后调用）
pub fn save_da_session(vid: u16, pid: u16, hw_code: u16, target_config: u32) {
    let state = SessionState {
        usb_vid: vid,
        usb_pid: pid,
        hw_code,
        target_config,
        da_loaded: true,
    };
    if let Err(e) = state.save() {
        warn!("[session] 保存 .state 失败: {}", e);
    } else {
        debug!("[session] DA 会话已保存");
    }
}

/// 重置设备并清除 .state
pub fn reset_session() {
    SessionState::remove();
    info!("[session] 会话已重置");
}
