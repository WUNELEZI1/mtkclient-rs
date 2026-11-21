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
use crate::transport::RetryPolicy;
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
    /// CID 寄存器（eMMC 查询成功时非空），用于审计与换件核对。
    pub cid: Vec<u8>,
    /// 当前活动槽（A/B），由 GPT 槽位探测填充。
    pub slot: Option<char>,
}

/// 分区定位结果（能力驱动路径统一用字节地址，不暴露 GPT 细节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionSpan {
    pub addr: u64,
    pub size: u64,
    /// 8 = 用户区（GPT 分区），0/1 = boot1/boot2
    pub parttype: u32,
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

    // ── 分区 IO（读 / 写 / 擦） ─────────────────────────────
    //
    // 这些方法是 ops 层唯一的数据通道：真机走 DAXFlash，仿真走 da::engine，
    // 因此同一份 `ops::write` 代码在两边的行为完全同构。

    /// 定位分区（含特殊分区 boot1/boot2）。
    fn find_partition(&mut self, name: &str) -> Result<PartitionSpan, String>;

    /// 按地址读取（一次调用可能很大，由后端自行分块）。
    fn read_flash(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String>;

    /// 按地址写入一段内存数据（seccfg / vbmeta / 小镜像）。
    fn write_flash(&mut self, addr: u64, data: &[u8]) -> Result<(), String>;

    /// 流式写入：从 `src` 读 `total` 字节写到 `addr + start`，
    /// 每写入一块回调 `on_progress(已写入总量)`（断点续传的持久化点）。
    ///
    /// `total` 是**本次要写的总长度**（从 offset 0 起算，含已写部分）。
    fn write_stream(
        &mut self,
        addr: u64,
        src: &mut dyn std::io::Read,
        total: u64,
        start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), String>;

    /// 按分区名擦除（后端自行解析地址）。返回擦除字节数。
    fn erase_partition(&mut self, name: &str) -> Result<u64, String>;

    /// 活动 A/B 槽位。
    ///
    /// - `Ok(Some(c))`：确定的活动槽；
    /// - `Ok(None)`：非 A/B 设备；
    /// - `Err(_)`：**无法判定**（A/B 但 bootctrl 读不到）——调用方必须报错，不能猜。
    fn active_slot(&mut self) -> Result<Option<char>, String>;

    /// 用设备侧密钥对 seccfg 头部摘要签名（V4 的 32 字节哈希尾）。
    fn seccfg_sign(&mut self, digest: &[u8; 32], hwtype: &str) -> Result<Vec<u8>, String>;

    /// 设备序列号（危险操作确认 token 用）。
    fn serial(&mut self) -> Option<String> {
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
    /// 已打开的存储信息（写/擦时的 storage 参数）
    storage: Option<crate::da::engine::StorageInfo>,
    /// 瞬时超时有界重试策略（重连/重载状态后仍保留）
    retry: RetryPolicy,
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
            storage: None,
            retry: RetryPolicy::default(),
        }
    }

    /// 设置瞬时超时重试策略（来自 `--max-retries`）。
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self.engine = std::mem::take(&mut self.engine).with_retry(policy);
        self
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

    /// 保存虚拟设备的 flash 状态（跨进程续传演示用）。
    pub fn save_state(&self, path: &str) -> Result<(), String> {
        self.dev.save_state(path)
    }

    /// 载入虚拟设备的 flash 状态。
    pub fn load_state(&mut self, path: &str) -> Result<(), String> {
        self.dev.load_state(path)?;
        self.engine = DaEngine::new().with_retry(self.retry);
        self.gpt = None;
        self.info = None;
        self.storage = None;
        Ok(())
    }

    #[cfg(test)]
    /// 模拟拔插重连：协议状态清零、flash 内容保留（断点续传测试用）。
    pub fn replug(&mut self) {
        self.dev.replug();
        self.engine = DaEngine::new().with_retry(self.retry);
        self.gpt = None;
        self.info = None;
        self.storage = None;
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
        // 查询失败不阻断（老 DA 不支持），此时容量字段为 0 并已打警告
        let storage = self
            .engine
            .query_storage(&mut self.dev)
            .unwrap_or_else(|_| crate::da::engine::StorageInfo {
                block: 512,
                ..Default::default()
            });
        self.engine
            .open_storage(&mut self.dev)
            .map_err(SimSession::map_err)?;
        self.storage = Some(storage);
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
            cid: Vec::new(),
            slot: self.state.current_slot,
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

    fn find_partition(&mut self, name: &str) -> Result<PartitionSpan, String> {
        // boot1/boot2 是特殊分区（不在 GPT 里）
        let (parttype, special_size) = match name {
            "boot1" | "boot2" => {
                let st = self.ensure_storage()?;
                let size = if name == "boot1" { st.boot1 } else { st.boot2 };
                (if name == "boot1" { 0u32 } else { 1u32 }, size)
            }
            _ => (8u32, 0u64),
        };
        if self.gpt.is_none() {
            self.parse_gpt()?;
        }
        let gpt = self.gpt.clone().ok_or_else(|| "GPT 不可用".to_string())?;
        let info = crate::partition::GptInfo::parse(&gpt)?;
        let entry = info
            .find_partition(name)
            .ok_or_else(|| format!("未找到分区: {}", name))?;
        let size = if special_size > 0 {
            special_size
        } else {
            entry.size
        };
        Ok(PartitionSpan {
            addr: entry.start_addr,
            size,
            parttype,
        })
    }

    fn read_flash(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        // 单次 READ_DATA 上限 1 MiB（真机 DA 同样按包长分块）
        let mut out = Vec::with_capacity(size as usize);
        let mut pos = 0u64;
        while pos < size {
            let take = (size - pos).min(0x10_0000);
            let chunk = self
                .engine
                .read_flash(&mut self.dev, addr + pos, take)
                .map_err(SimSession::map_err)?;
            out.extend_from_slice(&chunk);
            pos += take;
        }
        Ok(out)
    }

    fn write_flash(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        let storage = self.storage_type();
        self.engine
            .begin_write(&mut self.dev, addr, data.len() as u64, storage, 8)
            .map_err(SimSession::map_err)?;
        let mut pos = 0usize;
        while pos < data.len() {
            let take = (data.len() - pos).min(0x10_0000);
            let mut chunk = data[pos..pos + take].to_vec();
            while !chunk.len().is_multiple_of(512) {
                chunk.push(0);
            }
            self.engine
                .write_chunk(&mut self.dev, &chunk)
                .map_err(SimSession::map_err)?;
            pos += take;
        }
        self.engine
            .end_write(&mut self.dev)
            .map_err(SimSession::map_err)
    }

    fn write_stream(
        &mut self,
        addr: u64,
        src: &mut dyn std::io::Read,
        total: u64,
        start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), String> {
        let storage = self.storage_type();
        self.engine
            .begin_write(&mut self.dev, addr + start, total - start, storage, 8)
            .map_err(SimSession::map_err)?;
        let mut pos = start;
        let mut buf = vec![0u8; 0x10_0000];
        while pos < total {
            let want = ((total - pos) as usize).min(buf.len());
            let mut n = 0usize;
            while n < want {
                let r = src
                    .read(&mut buf[n..want])
                    .map_err(|e| format!("读取源数据失败: {}", e))?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n == 0 {
                return Err(format!("源数据提前结束: 期望 {} 字节", total - pos));
            }
            let mut chunk = buf[..n].to_vec();
            while !chunk.len().is_multiple_of(512) {
                chunk.push(0);
            }
            self.engine
                .write_chunk(&mut self.dev, &chunk)
                .map_err(SimSession::map_err)?;
            pos += n as u64;
            on_progress(pos);
        }
        self.engine
            .end_write(&mut self.dev)
            .map_err(SimSession::map_err)
    }

    fn erase_partition(&mut self, name: &str) -> Result<u64, String> {
        let span = self.find_partition(name)?;
        if span.size == 0 {
            return Err(format!("分区 {} 大小为 0", name));
        }
        self.engine
            .format(&mut self.dev, span.parttype, span.addr, span.size)
            .map_err(SimSession::map_err)?;
        Ok(span.size)
    }

    fn active_slot(&mut self) -> Result<Option<char>, String> {
        if self.gpt.is_none() {
            self.parse_gpt()?;
        }
        let has_a = self.partition_exists("boot_a") || self.partition_exists("vbmeta_a");
        let has_b = self.partition_exists("boot_b") || self.partition_exists("vbmeta_b");
        if !has_a && !has_b {
            return Ok(None);
        }
        // bootctrl 可读性由画像决定（真机由 misc 的 boot_control 结构决定）
        if self.dev.profile.bootctrl
            && let Some(s) = self.dev.profile.slot
        {
            return Ok(Some(s));
        }
        match (has_a, has_b) {
            (true, true) => Err(
                "无法确定活动 A/B 槽位（bootctrl 读取失败，且 GPT 中 _a/_b 同时存在）".to_string(),
            ),
            (true, false) => Ok(Some('a')),
            _ => Ok(Some('b')),
        }
    }

    fn seccfg_sign(&mut self, digest: &[u8; 32], _hwtype: &str) -> Result<Vec<u8>, String> {
        // 仿真设备不校验 seccfg 哈希，直接返回明文摘要（sim-only；真机走 HACC/SEJ）
        Ok(digest.to_vec())
    }
}

impl SimSession {
    fn ensure_storage(&mut self) -> Result<crate::da::engine::StorageInfo, String> {
        if let Some(s) = self.storage {
            return Ok(s);
        }
        let s = self.engine.query_storage(&mut self.dev).unwrap_or_default();
        self.storage = Some(s);
        Ok(s)
    }

    fn storage_type(&self) -> u32 {
        self.storage.map(|s| s.storage_type()).unwrap_or(0)
    }

    fn partition_exists(&self, name: &str) -> bool {
        self.gpt
            .as_ref()
            .and_then(|g| crate::partition::GptInfo::parse(g).ok())
            .map(|i| i.find_partition(name).is_some())
            .unwrap_or(false)
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
