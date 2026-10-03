//! DA 两阶段上传
//!
//! - `upload_da1` — Stage1：发送 DA1 + jump_da + 同步握手
//! - `upload_da2` — Stage2：发送 DA2 + boot_to

use log::{info, trace, warn};
// File/Read 不再需要，DA 文件通过 read_file_auto_decompress 读取
use std::time::Duration;

use crate::da::loader::header::DaRegion;
use crate::da::xflash::DAXFlash;
use crate::exploit::carbonara::Carbonara;
use crate::system::paths::get_exe_relative_path;

use super::header::parse_da_header;

const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const DEFAULT_DA_FILE: &str = "MTK_DA_V5.bin";

impl<'a> DAXFlash<'a> {
    /// 打开 DA 文件路径：优先使用用户指定的路径（--da 参数），否则使用默认路径
    fn open_da_path(&self) -> (std::path::PathBuf, String) {
        let path = if !self.preloader.da_path.is_empty() {
            std::path::PathBuf::from(&self.preloader.da_path)
        } else {
            get_exe_relative_path(DEFAULT_DA_FILE)
        };
        let path_str = path.to_string_lossy().to_string();
        (path, path_str)
    }

    /// 获取或缓存 DA 文件数据（支持 .bin.gz 自动解压）
    fn load_da_file(&mut self) -> Result<Vec<u8>, String> {
        if let Some(ref data) = self.da_file_data {
            trace!("[DA_CACHE] 复用缓存的 DA 文件数据 ({} 字节)", data.len());
            return Ok(data.clone());
        }
        let (path, _path_str) = self.open_da_path();
        let da_data = crate::system::compress::read_file_auto_decompress(&path)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;
        trace!("[DA_CACHE] 读取 DA 文件并缓存 ({} 字节)", da_data.len());
        self.da_file_data = Some(da_data.clone());
        Ok(da_data)
    }

    /// 尝试从后台预解析结果获取 DA 数据和 regions
    /// init() 中已用正确 hw code 完成解析，upload_da 时直接取用，无需重新解析
    fn load_da_with_regions(&mut self) -> Result<(Vec<u8>, Vec<DaRegion>), String> {
        // 优先使用后台线程预解析结果（含 header 解析）
        if let Some(ref parsed_arc) = self.preloader.da_parsed {
            if let Ok(guard) = parsed_arc.lock() {
                if let Some((data, regions)) = guard.as_ref() {
                    trace!(
                        "[DA_PRELOAD] 使用后台线程预解析的 DA 数据 ({} 字节, {} regions)",
                        data.len(),
                        regions.len()
                    );
                    self.da_file_data = Some(data.clone());
                    return Ok((data.clone(), regions.clone()));
                }
            }
        }

        // Fallback：同步读取 + 解析（使用芯片 da_code 匹配 DA 文件内条目）
        trace!("[DA_PRELOAD] 后台线程未就绪，回退到同步加载");
        let da_data = self.load_da_file()?;
        let da_code = self
            .preloader
            .chip
            .as_ref()
            .map(|c| c.da_code)
            .ok_or_else(|| "芯片配置不可用，无法确定 DA code".to_string())?;
        let (_magic, regions, _is_v6) = parse_da_header(&da_data, da_code)?;
        Ok((da_data, regions))
    }

    /// 上传第一阶段 DA
    /// 对照 Python xflash_lib.py:upload_da1
    pub fn upload_da1(&mut self) -> Result<bool, String> {
        trace!("上传 XFlash 阶段 1...");

        let (da_data, regions) = self.load_da_with_regions()?;

        if regions.len() < 2 {
            return Err("DA 文件格式错误，无法找到 Stage1 region".to_string());
        }

        let stage1 = &regions[1];
        let da1_buf_offset = stage1.buf_offset;
        let da1_len = stage1.len;
        let da1_address = stage1.start_addr;
        let _da1_sig_len = stage1.sig_len;

        info!(
            "upload_da1: DA 文件大小={} bytes (0x{:X}), DA1 地址=0x{:08X}, DA1 大小={} bytes (0x{:X})",
            da_data.len(),
            da_data.len(),
            da1_address,
            da1_len,
            da1_len
        );

        let da1_start = da1_buf_offset as usize;
        let da1_end = da1_start + da1_len as usize;
        if da1_end > da_data.len() {
            return Err("DA 文件格式错误，Stage1 数据超出文件范围".to_string());
        }

        let mut da1_patched = da_data[da1_start..da1_end].to_vec();
        trace!("应用 DA1 patch...");
        Self::patch_da1(&mut da1_patched);

        if !self
            .preloader
            .send_da(da1_address, da1_len, 0x100, &da1_patched)?
        {
            return Err("发送 DA 失败".to_string());
        }

        trace!("成功上传 stage 1，跳转中...");

        // jump_da 内部已消费 0xC0 同步信号（含跳过 0x00 填充），无需再次读取
        self.preloader.jump_da(da1_address)?;

        // Python: self.sync() 发送 XFlash SYNC_SIGNAL
        self.xflash_sync()?;

        // Python: self.setup_env()
        self.setup_env()?;

        // DA1 刚跳转完成，设备需要时间准备，给予 100ms 缓冲（release 模式优化后可能过快）
        std::thread::sleep(Duration::from_millis(100));

        // Python: self.setup_hw_init()
        self.setup_hw_init()?;

        // 设备需要时间返回 xread 响应，用轮询代替固定 500ms
        // 先 flush_input 轮询等设备 ready，再 xread
        // 100 次 x 10ms = 最大 1s，缓冲区空则提前退出
        self.preloader
            .flush_input_poll(Duration::from_millis(10), 100)?;

        // Python: res = self.xread(); if res == pack("<I", self.Cmd.SYNC_SIGNAL)
        let resp = self.xread()?;
        if resp != CMD_SYNC_SIGNAL {
            return Err(format!("Error jumping to DA: got 0x{:08X}", resp));
        }
        info!("已接收 DA 同步信号");

        Ok(true)
    }

    /// 上传第二阶段 DA
    /// 流程：检查是否需要 EMI → 发送 EMI → 调用 boot_to 上传 Stage2 → reinit
    pub fn upload_da2(&mut self) -> Result<bool, String> {
        trace!("上传 XFlash 阶段 2...");

        let (da_data, regions) = self.load_da_with_regions()?;

        if regions.len() < 3 {
            return Err("DA 文件格式错误，无法找到 Stage2 region".to_string());
        }

        let stage2 = &regions[2];
        let da2_buf_offset = stage2.buf_offset;
        let da2_len = stage2.len;
        let da2_address = stage2.start_addr;
        let da2_sig_len = stage2.sig_len;

        trace!(
            "  偏移: 0x{:08X}, 大小: 0x{:08X}, 地址: 0x{:08X}",
            da2_buf_offset, da2_len, da2_address
        );

        let da2_start = da2_buf_offset as usize;
        let da2_end_raw = da2_start + da2_len as usize;
        if da2_end_raw > da_data.len() {
            return Err("DA 文件格式错误，Stage2 数据超出文件范围".to_string());
        }

        let sig_len = da2_sig_len as usize;
        let da2_size_before = da2_end_raw - da2_start;
        let mut da2_data = if sig_len > 0 && da2_size_before > sig_len {
            da_data[da2_start..da2_end_raw - sig_len].to_vec()
        } else {
            da_data[da2_start..da2_end_raw].to_vec()
        };
        if sig_len > 0 {
            trace!(
                "  DA2 原始大小: 0x{:X} ({}) 字节",
                da2_size_before, da2_size_before
            );
            trace!(
                "  DA2 截断后大小: 0x{:X} ({}) 字节 (已截断 0x{:X} 字节签名)",
                da2_data.len(),
                da2_data.len(),
                sig_len
            );
        }

        trace!("应用 DA2 patch...");
        Self::patch_da2(&mut da2_data);

        self.da2_data = da2_data.clone();
        self.da2_base_addr = da2_address as u64;

        // 常规路径：boot_to 发送 DA2 并跳转。成功/失败语义与原来保持一致。
        match self.boot_to(da2_address, &da2_data, true, 0.5) {
            Ok(true) => {}
            Ok(false) => return Err("上传 Stage2 失败".to_string()),
            Err(e) => {
                // 常规 DA2 上传失败（例如 DA1 的 DA2 hash 校验拒绝未签名/已修改的 DA2）时，
                // 回退到 Carbonara（DA2 hash 欺骗）漏洞利用路径重试一次。
                warn!("常规 DA2 上传失败 ({}), 尝试 Carbonara fallback...", e);
                self.carbonara_upload_da2(&da_data, &regions, &da2_data)
                    .map_err(|ce| format!("{}; Carbonara fallback 也失败: {}", e, ce))?;
            }
        }

        trace!("Stage2 上传成功");
        Ok(true)
    }

    /// Carbonara（DA2 hash 欺骗）fallback：常规 `boot_to` 上传 DA2 失败时，
    /// 通过 DA1 的 `boot_to` 参数覆写其内存中的 DA2 hash 存储位置，
    /// 从而让 hash 校验通过并加载自定义（已 patch）的 DA2。
    ///
    /// `patch_and_upload` 内部会自行检测 DA1 patch 状态与 hash 校验逻辑
    /// （已打补丁或未检测到 hash 校验时直接返回错误），因此该 fallback 是安全的。
    fn carbonara_upload_da2(
        &mut self,
        da_data: &[u8],
        regions: &[DaRegion],
        da2_data: &[u8],
    ) -> Result<(), String> {
        let mut carbonara =
            Carbonara::new(da_data, regions).map_err(|e| format!("Carbonara 初始化失败: {}", e))?;
        info!(
            "Carbonara: DA1 @ 0x{:08X} ({} 字节), DA2 @ 0x{:08X} ({} 字节)",
            carbonara.da1_addr(),
            carbonara.da1_data().len(),
            carbonara.da2_addr(),
            carbonara.da2_data().len()
        );
        // 检测 DA1 是否已打补丁 / 是否含 hash 校验逻辑
        carbonara.check_patched()?;
        let mut log_sink = std::io::sink();
        carbonara.patch_and_upload(&mut *self.preloader.device, &mut log_sink, Some(da2_data))
    }
}
