//! Two-stage DA upload
//!
//! - `upload_da1` — Stage1: send DA1 + jump_da + sync handshake
//! - `upload_da2` — Stage2: send DA2 + boot_to

use log::{info, trace, warn};
// File/Read are no longer needed; DA files are read via read_file_auto_decompress
use std::time::Duration;

use crate::da::loader::header::DaRegion;
use crate::da::xflash::DAXFlash;
use crate::exploit::carbonara::Carbonara;
use crate::system::paths::get_exe_relative_path;

use super::header::parse_da_header;

const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const DEFAULT_DA_FILE: &str = "MTK_DA_V5.bin";

impl<'a> DAXFlash<'a> {
    /// Resolve the DA file path: use the user-specified path (--da) if any, else the default path
    fn open_da_path(&self) -> (std::path::PathBuf, String) {
        let path = if !self.preloader.da_path.is_empty() {
            std::path::PathBuf::from(&self.preloader.da_path)
        } else {
            get_exe_relative_path(DEFAULT_DA_FILE)
        };
        let path_str = path.to_string_lossy().to_string();
        (path, path_str)
    }

    /// Fetch or cache DA file data (auto-decompresses .bin.gz)
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

    /// Try to take DA data and regions from the background pre-parse result
    /// init() already parsed with the correct hw code, so upload_da can reuse it without re-parsing
    fn load_da_with_regions(&mut self) -> Result<(Vec<u8>, Vec<DaRegion>), String> {
        // Prefer the background-thread pre-parse result (including header parsing)
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

        // Fallback: synchronous read + parse (use the chip da_code to match entries in the DA file)
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

    /// Upload the Stage1 DA
    /// Mirrors Python xflash_lib.py:upload_da1
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

        // jump_da already consumes the 0xC0 sync signal (skipping 0x00 padding), so no re-read is needed
        self.preloader.jump_da(da1_address)?;

        // Python: self.sync() sends XFlash SYNC_SIGNAL
        self.xflash_sync()?;

        // Python: self.setup_env()
        self.setup_env()?;

        // DA1 has just jumped; the device needs time to settle, so allow a 100ms buffer (release builds may be too fast)
        std::thread::sleep(Duration::from_millis(100));

        // Python: self.setup_hw_init()
        self.setup_hw_init()?;

        // The device needs time to return the xread response; poll instead of waiting a fixed 500ms
        // First poll with flush_input until the device is ready, then xread
        // 100 × 10ms = at most 1s; exits early when the buffer is empty
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

    /// Upload the Stage2 DA
    /// Flow: check whether EMI is needed → send EMI → call boot_to to upload Stage2 → reinit
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

        // Normal path: boot_to sends DA2 and jumps. Success/failure semantics are unchanged.
        match self.boot_to(da2_address, &da2_data, true, 0.5) {
            Ok(true) => {}
            Ok(false) => return Err("上传 Stage2 失败".to_string()),
            Err(e) => {
                // When the normal DA2 upload fails (e.g. DA1's DA2 hash check rejects an unsigned/modified DA2),
                // fall back to the Carbonara (DA2 hash spoofing) exploit path and retry once.
                warn!("常规 DA2 上传失败 ({}), 尝试 Carbonara fallback...", e);
                self.carbonara_upload_da2(&da_data, &regions, &da2_data)
                    .map_err(|ce| format!("{}; Carbonara fallback 也失败: {}", e, ce))?;
            }
        }

        trace!("Stage2 上传成功");
        Ok(true)
    }

    /// Carbonara (DA2 hash spoofing) fallback: when the normal `boot_to` DA2 upload fails,
    /// overwrite the DA2 hash storage location in DA1 memory via its `boot_to` argument,
    /// so the hash check passes and the custom (patched) DA2 is loaded.
    ///
    /// `patch_and_upload` detects the DA1 patch state and hash-check logic itself
    /// (it returns an error if already patched or no hash check is found), so this fallback is safe.
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
        // Check whether DA1 is already patched / contains hash-check logic
        carbonara.check_patched()?;
        let mut log_sink = std::io::sink();
        carbonara.patch_and_upload(&mut *self.preloader.device, &mut log_sink, Some(da2_data))
    }
}
