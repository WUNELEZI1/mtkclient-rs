//! 真实设备会话：把既有 `DAXFlash` 全流程适配到 [`Session`]。
//!
//! 这里**不重写**任何协议逻辑，只做"步骤 → 既有方法"的映射，
//! 因此接入 Orchestrator 不会改变真机行为。

use super::{DeviceInfo, PartitionSpan, Session};

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
            cid: info.cid,
            slot: self.state.current_slot,
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

    fn find_partition(&mut self, name: &str) -> Result<PartitionSpan, String> {
        // 特殊分区（boot1/boot2）走 DAXFlash 的既有解析，parttype 不是 8
        if let Some((parttype, addr, size)) = self.da.resolve_special_partition(name) {
            return Ok(PartitionSpan {
                addr,
                size,
                parttype,
            });
        }
        if self.da.last_gpt_data.is_none() && !self.da.try_load_cached_gpt() {
            self.da.read_gpt()?;
        }
        let (addr, size) = self.da.find_partition_addr(name)?;
        Ok(PartitionSpan {
            addr,
            size,
            parttype: 8,
        })
    }

    fn read_flash(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        self.da.readflash_data(addr, size)
    }

    fn write_flash(&mut self, addr: u64, data: &[u8]) -> Result<(), String> {
        self.da.write_flash_data(addr, data, 1, 8)
    }

    fn write_stream(
        &mut self,
        addr: u64,
        src: &mut dyn std::io::Read,
        total: u64,
        start: u64,
        on_progress: &mut dyn FnMut(u64),
    ) -> Result<(), String> {
        // 复用既有流式写入（含进度条/包长协商）。断点续传由 ops 层的 checkpoint
        // 负责持久化，因此这里不传 resume_input（空串 = 不写 .wresume）。
        // `on_progress` 通过包装 Reader 触发：每读满一块即回调。
        struct ProgressReader<'a, 'f> {
            inner: &'a mut dyn std::io::Read,
            done: u64,
            cb: &'f mut dyn FnMut(u64),
        }
        impl std::io::Read for ProgressReader<'_, '_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.inner.read(buf)?;
                if n > 0 {
                    self.done += n as u64;
                    (self.cb)(self.done);
                }
                Ok(n)
            }
        }
        let mut reader = ProgressReader {
            inner: src,
            done: start,
            cb: on_progress,
        };
        self.da
            .write_flash_data_stream(addr + start, &mut reader, total - start, 1, 8, start, "")
    }

    fn erase_partition(&mut self, name: &str) -> Result<u64, String> {
        let span = self.find_partition(name)?;
        self.da.erase_partition(name)?;
        Ok(span.size)
    }

    fn active_slot(&mut self) -> Result<Option<char>, String> {
        crate::security::vbmeta::resolve_slot(self.da).map(|s| match s {
            crate::security::vbmeta::SlotState::Known(c) => Some(c),
            crate::security::vbmeta::SlotState::NotAb => None,
        })
    }

    fn seccfg_sign(&mut self, digest: &[u8; 32], hwtype: &str) -> Result<Vec<u8>, String> {
        let hw_code = self
            .da
            .preloader
            .chip
            .as_ref()
            .map(|c| c.hw_code)
            .ok_or_else(|| "芯片配置不可用，无法执行 HACC 签名".to_string())?;
        crate::security::seccfg::build::sign_v4_digest(self.da, hwtype, digest, hw_code)
    }

    fn serial(&mut self) -> Option<String> {
        // 优先用 CID（eMMC 唯一），退化到芯片名 + hw_code
        if let Some(chip) = self.da.preloader.chip.as_ref() {
            return Some(format!("{}-{:04X}", chip.name, chip.hw_code));
        }
        None
    }
}
