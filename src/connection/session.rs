use log::{info, trace, warn};
use std::collections::HashMap;
use std::fmt;
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
    /// 上次成功 dump 的 preloader 文件路径，避免重复 dump
    pub preloader_path: Option<String>,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "usb_vid=0x{:04X}\nusb_pid=0x{:04X}\nhw_code=0x{:04X}\ntarget_config=0x{:08X}\nda_loaded={}",
            self.usb_vid, self.usb_pid, self.hw_code, self.target_config, self.da_loaded
        )?;
        if let Some(ref path) = self.preloader_path {
            write!(f, "\npreloader_path={}", path)?;
        }
        Ok(())
    }
}

impl SessionState {
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
            target_config: u32::from_str_radix(map.get("target_config")?.strip_prefix("0x")?, 16)
                .ok()?,
            da_loaded: map.get("da_loaded")?.parse::<bool>().ok()?,
            preloader_path: map.get("preloader_path").map(|s| s.to_string()),
        })
    }

    /// 写入 .state 文件
    pub fn save(&self) -> Result<(), String> {
        fs::write(STATE_FILE, self.to_string()).map_err(|e| format!("写入 .state 失败: {}", e))
    }

    /// 读取 .state 文件
    pub fn load() -> Option<Self> {
        if !Path::new(STATE_FILE).exists() {
            return None;
        }
        match fs::read_to_string(STATE_FILE) {
            Ok(content) => {
                let state = Self::from_string(&content)?;
                trace!(
                    "[session] .state 已加载: hw_code=0x{:04X}, da_loaded={}",
                    state.hw_code, state.da_loaded
                );
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
            trace!("[session] .state 已删除");
        }
    }

    /// 检查设备是否在线（VID/PID 匹配）
    pub fn device_online(&self, vid: u16, pid: u16) -> bool {
        self.usb_vid == vid && self.usb_pid == pid
    }
}

/// 尝试复用现有 DA 会话
/// 如果 .state 存在且设备已处于 DA 模式（PID=0x2000 / 0x0005），跳过 BROM→DA 流程
pub fn try_reuse_da_session(vid: u16, pid: u16) -> bool {
    // 核心安全检查：只有当设备明确处于 DA 模式时，才允许复用
    // PID 0x2000 是标准的 DA 模式 PID
    // PID 0x0005 是某些旧芯片或特定 DA 的 PID
    if pid != 0x2000 && pid != 0x0005 {
        trace!(
            "[session] 设备 PID=0x{:04X} 不属于 DA 模式，拒绝复用会话",
            pid
        );
        return false;
    }

    if let Some(state) = SessionState::load() {
        if state.da_loaded && state.device_online(vid, pid) {
            info!(
                "[session] 复用 DA 会话（hw_code=0x{:04X}，设备在线）",
                state.hw_code
            );
            return true;
        } else {
            trace!("[session] .state 存在但设备 PID/VID 不匹配或 DA 未加载，重新初始化");
        }
    }
    false
}

/// 保存 DA 会话状态（在 DA 加载成功后调用）
pub fn save_da_session(
    vid: u16,
    pid: u16,
    hw_code: u16,
    target_config: u32,
    preloader_path: Option<&str>,
) {
    let state = SessionState {
        usb_vid: vid,
        usb_pid: pid,
        hw_code,
        target_config,
        da_loaded: true,
        preloader_path: preloader_path.map(|s| s.to_string()),
    };
    if let Err(e) = state.save() {
        warn!("[session] 保存 .state 失败: {}", e);
    } else {
        trace!("[session] DA 会话已保存");
    }
}

/// 重置设备并清除 .state
pub fn reset_session() {
    SessionState::remove();
    info!("[session] 会话已重置");
}
