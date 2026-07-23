use log::{info, trace, warn};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

/// DA 会话状态文件路径
const STATE_FILE: &str = ".state";

/// DA 会话最大存活时间（秒），超过此时间 .state 视为过期
const SESSION_MAX_AGE_SECS: u64 = 300; // 5 分钟

/// DA 初始化模式：记录 DA 是通过哪种路径加载的，
/// 会话复用时根据此字段选择对应的重握手方案
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    /// BROM 模式加载：握手 → 关看门狗 → bypass → send_da → setup_env → setup_hw_init
    Brom,
    /// Preloader 模式加载：跳过 bypass，直接 send_da → setup_env → setup_hw_init
    Preloader,
}

impl InitMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "brom" => Some(InitMode::Brom),
            "preloader" => Some(InitMode::Preloader),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            InitMode::Brom => "brom",
            InitMode::Preloader => "preloader",
        }
    }
}

/// 会话状态（对齐 Python .state 文件机制）
#[derive(Debug, Clone)]
pub struct SessionState {
    pub usb_vid: u16,
    pub usb_pid: u16,
    pub hw_code: u16,
    pub target_config: u32,
    pub da_loaded: bool,
    /// 设备指纹：由 VID/PID、HW code、target_config、preloader 文件内容摘要组成
    pub device_fingerprint: Option<String>,
    /// 上次成功 dump 的 preloader 文件路径，避免重复 dump
    pub preloader_path: Option<String>,
    /// GPT 缓存文件路径，DA 会话复用时可避免重复读取 GPT
    pub gpt_cache_path: Option<String>,
    /// 当前 DA 会话中已知失败的可选查询，避免每次命令重复等待 timeout
    pub optional_query_failures: Vec<String>,
    /// 会话创建的 unix 时间戳（秒）
    pub created_at: u64,
    /// DA 初始化模式（brom/preloader），会话复用时决定重握手方案
    pub init_mode: InitMode,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "usb_vid=0x{:04X}\nusb_pid=0x{:04X}\nhw_code=0x{:04X}\ntarget_config=0x{:08X}\nda_loaded={}\ncreated_at={}\ninit_mode={}",
            self.usb_vid,
            self.usb_pid,
            self.hw_code,
            self.target_config,
            self.da_loaded,
            self.created_at,
            self.init_mode.as_str()
        )?;
        if let Some(ref path) = self.preloader_path {
            write!(f, "\npreloader_path={}", path)?;
        }
        if let Some(ref fingerprint) = self.device_fingerprint {
            write!(f, "\ndevice_fingerprint={}", fingerprint)?;
        }
        if let Some(ref path) = self.gpt_cache_path {
            write!(f, "\ngpt_cache_path={}", path)?;
        }
        if !self.optional_query_failures.is_empty() {
            write!(
                f,
                "\noptional_query_failures={}",
                self.optional_query_failures.join(",")
            )?;
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

        let optional_query_failures = map
            .get("optional_query_failures")
            .map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_else(Vec::new);

        Some(SessionState {
            usb_vid: u16::from_str_radix(map.get("usb_vid")?.strip_prefix("0x")?, 16).ok()?,
            usb_pid: u16::from_str_radix(map.get("usb_pid")?.strip_prefix("0x")?, 16).ok()?,
            hw_code: u16::from_str_radix(map.get("hw_code")?.strip_prefix("0x")?, 16).ok()?,
            target_config: u32::from_str_radix(map.get("target_config")?.strip_prefix("0x")?, 16)
                .ok()?,
            da_loaded: map.get("da_loaded")?.parse::<bool>().ok()?,
            preloader_path: map.get("preloader_path").map(|s| s.to_string()),
            device_fingerprint: map.get("device_fingerprint").map(|s| s.to_string()),
            gpt_cache_path: map.get("gpt_cache_path").map(|s| s.to_string()),
            optional_query_failures,
            created_at: map
                .get("created_at")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0), // 兼容旧版无此字段的 .state 文件
            init_mode: map
                .get("init_mode")
                .and_then(|v| InitMode::from_str(v))
                .unwrap_or(InitMode::Brom), // 兼容旧版无此字段的 .state 文件
        })
    }

    /// 写入 .state 文件（原子写入：先写临时文件再 rename）
    pub fn save(&self) -> Result<(), String> {
        let tmp = format!("{}.tmp", STATE_FILE);
        fs::write(&tmp, self.to_string()).map_err(|e| format!("写入 .state.tmp 失败: {}", e))?;
        fs::rename(&tmp, STATE_FILE).map_err(|e| format!("rename .state.tmp 失败: {}", e))?;
        Ok(())
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

fn preloader_hash(path: Option<&str>) -> u64 {
    let Some(path) = path else {
        return 0;
    };
    let Ok(data) = fs::read(path) else {
        return 0;
    };
    data.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ (*byte as u64)).wrapping_mul(0x100000001b3)
    })
}

pub fn make_device_fingerprint(
    vid: u16,
    pid: u16,
    hw_code: u16,
    target_config: u32,
    preloader_path: Option<&str>,
) -> String {
    format!(
        "vid={:04X};pid={:04X};hw={:04X};tc={:08X};pre={:016X}",
        vid,
        pid,
        hw_code,
        target_config,
        preloader_hash(preloader_path)
    )
}

fn update_session_state<F>(mut update: F)
where
    F: FnMut(&mut SessionState),
{
    if let Some(mut state) = SessionState::load() {
        update(&mut state);
        if let Err(e) = state.save() {
            warn!("[session] 更新 .state 失败: {}", e);
        }
    }
}

/// 尝试复用现有 DA 会话
/// 如果 .state 存在且 da_loaded=true 且设备仍在线，跳过 BROM→DA 流程。
/// 不使用时间超时判断——只要设备保持连接，DA 会话就有效。
/// 真正的 DA 模式验证由后续的 check_da_session / reinit 完成（心跳检测）。
pub fn try_reuse_da_session(vid: u16, pid: u16) -> bool {
    if let Some(state) = SessionState::load() {
        if state.da_loaded && state.device_online(vid, pid) {
            info!(
                "[session] 复用 DA 会话（hw_code=0x{:04X}，设备在线）",
                state.hw_code,
            );
            return true;
        } else {
            trace!("[session] .state 存在但设备 PID/VID 不匹配或 DA 未加载，重新初始化");
            SessionState::remove();
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
    init_mode: InitMode,
) {
    let previous = SessionState::load();
    let device_fingerprint =
        make_device_fingerprint(vid, pid, hw_code, target_config, preloader_path);
    let same_device = previous
        .as_ref()
        .and_then(|state| state.device_fingerprint.as_ref())
        .map(|fingerprint| fingerprint == &device_fingerprint)
        .unwrap_or(false);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let state = SessionState {
        usb_vid: vid,
        usb_pid: pid,
        hw_code,
        target_config,
        da_loaded: true,
        device_fingerprint: Some(device_fingerprint),
        preloader_path: preloader_path.map(|s| s.to_string()),
        gpt_cache_path: if same_device {
            previous
                .as_ref()
                .and_then(|state| state.gpt_cache_path.clone())
        } else {
            None
        },
        // optional_query_failures 只根据 hw_code 判断，不区分 BROM/Preloader 模式
        // 因为同一台设备的可选查询失败情况不会因为模式切换而改变
        optional_query_failures: previous
            .filter(|state| state.hw_code == hw_code)
            .map(|state| state.optional_query_failures)
            .unwrap_or_default(),
        created_at: now,
        init_mode,
    };
    if let Err(e) = state.save() {
        warn!("[session] 保存 .state 失败: {}", e);
    } else {
        trace!("[session] DA 会话已保存");
    }
}

pub fn save_gpt_cache_path(path: &str) {
    update_session_state(|state| {
        state.gpt_cache_path = Some(path.to_string());
    });
}

pub fn get_gpt_cache_path() -> Option<String> {
    let state = SessionState::load()?;
    if state.device_fingerprint.is_none() {
        return None;
    }
    state.gpt_cache_path
}

pub fn optional_query_failed(name: &str) -> bool {
    SessionState::load()
        .map(|state| {
            state
                .optional_query_failures
                .iter()
                .any(|item| item == name)
        })
        .unwrap_or(false)
}

pub fn mark_optional_query_failed(name: &str) {
    update_session_state(|state| {
        if !state
            .optional_query_failures
            .iter()
            .any(|item| item == name)
        {
            state.optional_query_failures.push(name.to_string());
        }
    });
}

/// 重置设备并清除 .state
pub fn reset_session() {
    SessionState::remove();
    info!("[session] 会话已重置");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_state_roundtrips_gpt_cache_and_optional_failures() {
        let state = SessionState {
            usb_vid: 0x0E8D,
            usb_pid: 0x0003,
            hw_code: 0x6768,
            target_config: 0xE0,
            da_loaded: true,
            device_fingerprint: Some("vid=0E8D;pid=0003;hw=6768;tc=000000E0;pre=1".to_string()),
            preloader_path: Some("preloader.bin".to_string()),
            gpt_cache_path: Some("gpt.bin".to_string()),
            optional_query_failures: vec![
                "get_connection_agent".to_string(),
                "get_sla_status".to_string(),
            ],
            created_at: 1700000000,
            init_mode: InitMode::Brom,
        };

        let parsed = SessionState::from_string(&state.to_string()).unwrap();

        assert_eq!(parsed.gpt_cache_path.as_deref(), Some("gpt.bin"));
        assert!(parsed.device_fingerprint.is_some());
        assert!(
            parsed
                .optional_query_failures
                .contains(&"get_connection_agent".to_string())
        );
        assert!(
            parsed
                .optional_query_failures
                .contains(&"get_sla_status".to_string())
        );
    }

    #[test]
    fn device_fingerprint_changes_when_preloader_changes() {
        let a = make_device_fingerprint(0x0E8D, 0x0003, 0x6768, 0xE0, None);
        let b = make_device_fingerprint(0x0E8D, 0x0003, 0x6768, 0xE1, None);
        assert_ne!(a, b);
    }
}
