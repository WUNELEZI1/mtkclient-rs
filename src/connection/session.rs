use log::{info, trace, warn};
use std::collections::HashMap;
use std::fmt;
use std::fs;

use crate::error::SessionError;
use crate::system::paths::get_tmp_path;

/// Path of the DA session state file (under tmp/)
fn state_file_path() -> std::path::PathBuf {
    get_tmp_path(".state")
}

/// DA init mode: records which path loaded the DA;
/// session reuse picks the matching re-handshake plan from this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    /// Loaded via BROM: handshake → disable watchdog → bypass → send_da → setup_env → setup_hw_init
    Brom,
    /// Loaded via Preloader: skip bypass, send_da → setup_env → setup_hw_init directly
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

/// Session state (mirrors Python's .state file mechanism)
#[derive(Debug, Clone)]
pub struct SessionState {
    pub usb_vid: u16,
    pub usb_pid: u16,
    pub hw_code: u16,
    pub target_config: u32,
    pub da_loaded: bool,
    /// Device fingerprint: VID/PID, HW code, target_config, and a digest of the preloader file
    pub device_fingerprint: Option<String>,
    /// Path of the last successfully dumped preloader, avoiding a repeat dump
    pub preloader_path: Option<String>,
    /// GPT cache file path; avoids re-reading the GPT when a DA session is reused
    pub gpt_cache_path: Option<String>,
    /// Optional queries known to fail in this DA session, avoiding a timeout wait on every command
    pub optional_query_failures: Vec<String>,
    /// Session creation time as a Unix timestamp (seconds)
    pub created_at: u64,
    /// DA init mode (brom/preloader); determines the re-handshake plan on session reuse
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
    /// Deserialize from key=value format
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
                .unwrap_or(0), // compatible with older .state files lacking this field
            init_mode: map
                .get("init_mode")
                .and_then(|v| InitMode::from_str(v))
                .unwrap_or(InitMode::Brom), // compatible with older .state files lacking this field
        })
    }

    /// Write the .state file (atomic: write a temp file, then rename)
    pub fn save(&self) -> Result<(), String> {
        let state_path = state_file_path();
        let tmp = format!("{}.tmp", state_path.display());
        fs::write(&tmp, self.to_string()).map_err(|e| format!("写入 .state.tmp 失败: {}", e))?;
        fs::rename(&tmp, &state_path).map_err(|e| format!("rename .state.tmp 失败: {}", e))?;
        Ok(())
    }

    /// Read the .state file
    pub fn load() -> Option<Self> {
        let state_path = state_file_path();
        if !state_path.exists() {
            return None;
        }
        match fs::read_to_string(&state_path) {
            Ok(content) => {
                let Some(state) = Self::from_string(&content) else {
                    warn!(
                        "[session] {}",
                        SessionError::StateFileCorrupted(".state 内容无法解析".to_string())
                    );
                    return None;
                };
                trace!(
                    "[session] .state 已加载: hw_code=0x{:04X}, da_loaded={}",
                    state.hw_code, state.da_loaded
                );
                Some(state)
            }
            Err(e) => {
                warn!(
                    "[session] {}",
                    SessionError::StateFileCorrupted(e.to_string())
                );
                None
            }
        }
    }

    /// Delete the .state file
    pub fn remove() {
        let state_path = state_file_path();
        if state_path.exists() {
            let _ = fs::remove_file(&state_path);
            trace!("[session] .state 已删除");
        }
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

/// Save DA session state (call after the DA has loaded successfully)
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
    // Old session exists but the fingerprint changed → different device; old caches (GPT cache etc.) must be dropped
    if previous.is_some() && !same_device {
        warn!(
            "[session] {}（旧设备缓存将失效）",
            SessionError::DeviceMismatch(device_fingerprint.clone())
        );
    }
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
        // optional_query_failures depends only on hw_code, not on BROM/Preloader mode,
        // because a device's optional-query failures do not change when the mode switches
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

/// Reset the device and clear .state
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
                .iter()
                .any(|item| item == "get_connection_agent")
        );
        assert!(
            parsed
                .optional_query_failures
                .iter()
                .any(|item| item == "get_sla_status")
        );
    }

    #[test]
    fn device_fingerprint_changes_when_preloader_changes() {
        let a = make_device_fingerprint(0x0E8D, 0x0003, 0x6768, 0xE0, None);
        let b = make_device_fingerprint(0x0E8D, 0x0003, 0x6768, 0xE1, None);
        assert_ne!(a, b);
    }
}
