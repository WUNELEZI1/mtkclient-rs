//! 执行会话：把"规划出的步骤"落到具体后端。
//!
//! 两个实现：
//! - [`RealSession`] — 真实设备，复用既有 `DAXFlash` 全流程（USB / 串口）；
//! - [`SimSession`]  — 离线仿真，`da::engine` 状态机 + `transport::vdev` 虚拟设备。
//!
//! 二者实现同一 [`Session`] trait，因此 `ops` 层与 `exec` 层**只写一份**。
//! 这样"能力驱动"的执行路径在真机与仿真上完全同构，仿真里跑通的步骤序列
//! 就是真机会走的序列（协议差异由各自后端吸收）。

pub mod real;

pub use real::RealSession;

use crate::core::capability::{Capability, CapabilitySet};
use crate::core::device::{DeviceState, ProtocolVersion};
use crate::da::engine::{DaEngine, EngineError};
use crate::transport::vdev::VirtualDevice;

/// 设备摘要（用于表格表头 / 日志）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub hw_code: u16,
    pub da_code: u16,
    pub protocol: &'static str,
    pub boot1: u64,
    pub boot2: u64,
    pub user: u64,
    pub block: u32,
    pub emmc_type: String,
}

/// 一次执行所需的最小会话能力。
///
/// 方法刻意与 [`crate::core::plan::PlanStep`] 一一对应，Executor 只做"翻译"。
pub trait Session {
    /// 后端名（日志用）
    fn label(&self) -> &'static str;
    /// 规划输入：设备状态
    fn state(&self) -> DeviceState;
    /// 当前已具备的能力（必须真实反映后端状态，Executor 会校验）
    fn caps(&self) -> CapabilitySet;
    /// 可用于规划 / 上传的 DA 字节
    fn da(&self) -> Option<&[u8]>;
    /// DA 来源描述（进决策日志）
    fn da_source(&self) -> &'static str;

    /// 执行一个 exploit
    fn apply_exploit(&mut self, name: &'static str) -> Result<(), String>;
    /// 加载 DA（含握手 / 上传 / 跳转 / SYNC）
    fn load_da(&mut self) -> Result<(), String>;
    /// 打开存储（查询容量 + SET_STORAGE）
    fn open_storage(&mut self) -> Result<(), String>;
    /// 读取并解析 GPT（结果进缓存）
    fn parse_gpt(&mut self) -> Result<(), String>;

    /// 已读取的 GPT 原始字节
    fn gpt(&self) -> Option<&[u8]>;
    /// 设备信息（打印用）
    fn device_info(&mut self) -> Result<DeviceInfo, String>;

    /// super 动态分区元数据（能读到才返回 Some；仿真设备为 None）
    fn super_metadata(&mut self) -> Option<crate::partition::lp::SuperMetadata> {
        None
    }
}

// =============================================================================
// 仿真会话
// =============================================================================

/// 离线仿真会话。
pub struct SimSession {
    dev: VirtualDevice,
    engine: DaEngine,
    da: Option<Vec<u8>>,
    gpt: Option<Vec<u8>>,
    info: Option<DeviceInfo>,
    state: DeviceState,
    /// 设备侧已接受的 exploit 带来的能力（仿真里由设备确认"exploit 生效"）
    granted: CapabilitySet,
}

impl SimSession {
    pub fn new(dev: VirtualDevice) -> Self {
        let state = dev.profile.to_state();
        Self {
            dev,
            engine: DaEngine::new(),
            da: None,
            gpt: None,
            info: None,
            state,
            granted: CapabilitySet::EMPTY,
        }
    }

    /// 注入 DA 字节（仿真不会校验签名，但会走完整上传流程）。
    pub fn with_da(mut self, da: Vec<u8>) -> Self {
        self.da = Some(da);
        self
    }

    /// 设备侧事件日志（测试断言 / 诊断用）。
    pub fn events(&self) -> &[String] {
        &self.dev.events
    }

    fn map_err(e: EngineError) -> String {
        e.to_string()
    }
}

impl Session for SimSession {
    fn label(&self) -> &'static str {
        "sim"
    }

    fn state(&self) -> DeviceState {
        self.state.clone()
    }

    fn caps(&self) -> CapabilitySet {
        use crate::da::engine::Phase;
        let mut set = CapabilitySet::EMPTY;
        if matches!(self.engine.phase(), Phase::Da1 | Phase::Ready) {
            set.insert(Capability::DaLoaded);
            set.insert(if self.state.protocol == ProtocolVersion::V6 {
                Capability::DaXml
            } else {
                Capability::DaXFlash
            });
        }
        // 设备侧已清除安全标志时，等价于"BROM 绕过已完成"
        if self.engine.phase() != Phase::Idle && self.dev_bypassed() {
            set.insert(Capability::SafetyBypassed);
            set.insert(Capability::DaAuthBypass);
        }
        if self.engine.phase() == Phase::Ready {
            set.insert(Capability::StorageRead);
            set.insert(Capability::StorageWrite);
        }
        if self.gpt.is_some() {
            set.insert(Capability::GptParsed);
        }
        set.union(self.granted)
    }

    fn da(&self) -> Option<&[u8]> {
        self.da.as_deref()
    }

    fn da_source(&self) -> &'static str {
        if self.da.is_some() {
            "sim-injected"
        } else {
            "none"
        }
    }

    fn apply_exploit(&mut self, name: &'static str) -> Result<(), String> {
        // 握手必须先完成：真机上 exploit 也是 BROM 阶段产物
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        self.engine
            .sim_exploit(&mut self.dev, name)
            .map_err(SimSession::map_err)?;
        // 设备确认 exploit 生效 → 主机获得注册表声明的全部能力
        if let Some(meta) = crate::exploit::registry::ExploitRegistry::builtin().get(name) {
            self.granted = self.granted.union(meta.produces);
        }
        Ok(())
    }

    fn load_da(&mut self) -> Result<(), String> {
        let da = self
            .da
            .clone()
            .ok_or_else(|| "未提供 DA 文件（--da / --loader）".to_string())?;
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        self.engine
            .load_da(&mut self.dev, &da)
            .map_err(SimSession::map_err)?;
        // DA1 运行后做一次 extensions 握手（真机上这是 patch 后的必要步骤）
        let _ = self.engine.load_da_extensions(&mut self.dev);
        Ok(())
    }

    fn open_storage(&mut self) -> Result<(), String> {
        if self.engine.phase() == crate::da::engine::Phase::Idle {
            self.engine
                .connect(&mut self.dev)
                .map_err(SimSession::map_err)?;
        }
        let storage = self
            .engine
            .query_storage(&mut self.dev)
            .map_err(SimSession::map_err)?;
        self.engine
            .open_storage(&mut self.dev)
            .map_err(SimSession::map_err)?;
        self.info = Some(DeviceInfo {
            name: self.state.name.clone(),
            hw_code: self.state.hw_code,
            da_code: self.state.da_code,
            protocol: self.state.protocol.label(),
            boot1: storage.boot1,
            boot2: storage.boot2,
            user: storage.user,
            block: storage.block,
            emmc_type: storage.type_name().to_string(),
        });
        Ok(())
    }

    fn parse_gpt(&mut self) -> Result<(), String> {
        // 先读 32KB（主 GPT 头 + 分区项），不足时按需扩展
        let mut data = self
            .engine
            .read_flash(&mut self.dev, 0, 32768)
            .map_err(SimSession::map_err)?;
        let info = crate::partition::GptInfo::parse(&data)?;
        let need = 512 + info.num_part_entries as u64 * info.part_entry_size as u64;
        if need > data.len() as u64 {
            data = self
                .engine
                .read_flash(&mut self.dev, 0, need)
                .map_err(SimSession::map_err)?;
        }
        if &data[512..520] != b"EFI PART" {
            return Err("GPT 签名缺失".into());
        }
        self.gpt = Some(data);
        Ok(())
    }

    fn gpt(&self) -> Option<&[u8]> {
        self.gpt.as_deref()
    }

    fn device_info(&mut self) -> Result<DeviceInfo, String> {
        if let Some(i) = &self.info {
            return Ok(i.clone());
        }
        self.open_storage()?;
        self.info
            .clone()
            .ok_or_else(|| "无法获取设备信息".to_string())
    }
}

impl SimSession {
    fn dev_bypassed(&self) -> bool {
        // VirtualDevice 没有暴露 bypassed 字段，用事件日志判定：
        // 这是设备侧真实状态，而不是会话侧记账。
        self.dev
            .events
            .iter()
            .any(|e| e.contains("SIM exploit 接受"))
    }
}
