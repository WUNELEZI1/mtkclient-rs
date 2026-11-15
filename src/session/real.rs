//! 真实设备会话：把既有 `DAXFlash` 全流程适配到 [`Session`]。
//!
//! 这里**不重写**任何协议逻辑，只做"步骤 → 既有方法"的映射，
//! 因此接入 Orchestrator 不会改变真机行为。

use super::{DeviceInfo, Session};

use crate::core::capability::{Capability, CapabilitySet};
use crate::core::device::DeviceState;
use crate::da::DAXFlash;

/// 真实设备会话。
pub struct RealSession<'a, 'b> {
    da: &'a mut DAXFlash<'b>,
    /// 由调用方传入的规划视图（hW/da_code/协议/安全标志已在连接阶段得到）
    state: DeviceState,
    da_loaded: bool,
    storage_open: bool,
    gpt_parsed: bool,
    captured: Option<Vec<u8>>,
}

impl<'a, 'b> RealSession<'a, 'b> {
    /// `da_loaded` 表示调用方已完成 DA 上传（`handle_command` 的 Phase1 会做），
    /// 此时规划器只会规划"开存储 → 解析 GPT"，不会再要求 `da_load`。
    pub fn new(da: &'a mut DAXFlash<'b>, state: DeviceState, da_loaded: bool) -> Self {
        Self {
            da,
            state,
            da_loaded,
            storage_open: false,
            gpt_parsed: false,
            captured: None,
        }
    }
}

/// 真实设备的 exploit 执行：当前只有只读目标会用到 BROM exploit，
/// 且必须由上层（BROM 会话）执行——DA 会话里已无法回到 BROM。
fn unsupported_exploit(name: &str) -> String {
    format!(
        "exploit({}) 需要 BROM 会话，当前已在 DA 会话中；请以 --mode brom 重新连接后由规划器选择 BROM 路径",
        name
    )
}

impl Session for RealSession<'_, '_> {
    fn label(&self) -> &'static str {
        "usb"
    }

    fn state(&self) -> DeviceState {
        self.state.clone()
    }

    fn caps(&self) -> CapabilitySet {
        let mut set = CapabilitySet::EMPTY;
        if self.da_loaded {
            set.insert(Capability::DaLoaded);
        }
        if self.storage_open {
            set.insert(Capability::StorageRead);
            set.insert(Capability::StorageWrite);
        }
        if self.gpt_parsed {
            set.insert(Capability::GptParsed);
        }
        set
    }

    fn da(&self) -> Option<&[u8]> {
        None
    }

    fn da_source(&self) -> &'static str {
        "device"
    }

    fn apply_exploit(&mut self, name: &'static str) -> Result<(), String> {
        Err(unsupported_exploit(name))
    }

    fn load_da(&mut self) -> Result<(), String> {
        // 既有全流程：DA1 → 可选查询 → EMI → DA2 → reinit → DA extensions
        self.da.upload_da()?;
        self.da_loaded = true;
        Ok(())
    }

    fn open_storage(&mut self) -> Result<(), String> {
        // 真机上"打开存储"由 DA2 + CUSTOM_SET_STORAGE 完成，已在 upload_da 尾部做过；
        // 这里只做一次容量查询，确认存储可访问（失败不阻断，与 cmd_printgpt 一致）。
        let _ = self.da.get_emmc_info();
        self.storage_open = true;
        Ok(())
    }

    fn parse_gpt(&mut self) -> Result<(), String> {
        self.da.read_gpt()?;
        let data = self.da.get_last_gpt_data()?.clone();
        self.captured = Some(data);
        self.gpt_parsed = true;
        Ok(())
    }

    fn gpt(&self) -> Option<&[u8]> {
        self.captured.as_deref()
    }

    fn device_info(&mut self) -> Result<DeviceInfo, String> {
        let info = match self.da.get_emmc_info() {
            Ok(i) => i,
            Err(e) => {
                log::warn!("eMMC 信息查询失败（降级为简化查询）: {}", e);
                self.da.get_emmc_info_simple()?
            }
        };
        Ok(DeviceInfo {
            name: self.state.name.clone(),
            hw_code: self.state.hw_code,
            da_code: self.state.da_code,
            protocol: self.state.protocol.label(),
            boot1: info.boot1_size,
            boot2: info.boot2_size,
            user: info.user_size,
            block: info.block_size,
            emmc_type: info.emmc_type,
        })
    }

    fn super_metadata(&mut self) -> Option<crate::partition::lp::SuperMetadata> {
        if self.da.super_metadata.is_none() {
            if let Err(e) = self.da.ensure_super_metadata() {
                log::debug!("加载 super 动态分区元数据失败: {}", e);
            }
        }
        self.da.super_metadata.clone()
    }
}
