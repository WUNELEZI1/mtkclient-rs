//! DA 两阶段上传
//!
//! - `upload_da1` — Stage1：发送 DA1 + jump_da + 同步握手
//! - `upload_da2` — Stage2：发送 DA2 + boot_to

use log::{info, trace};
use std::fs::File;
use std::io::Read;
use std::time::Duration;

use crate::da_extension::DAXFlash;
use crate::system::paths::获取可执行文件相对路径;

use super::header::parse_da_header;

const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const DA_HW_CODE_MT6768: u16 = 0x6768;
const DEFAULT_DA_FILE: &str = "MTK_DA_V5.bin";

impl<'a> DAXFlash<'a> {
    fn open_da_file() -> Result<(File, String), String> {
        let path = 获取可执行文件相对路径(DEFAULT_DA_FILE);
        let path_str = path.to_string_lossy().to_string();
        let file =
            File::open(&path).map_err(|e| format!("无法打开 DA 文件 '{}': {}", path_str, e))?;
        Ok((file, path_str))
    }

    /// 获取或缓存 DA 文件数据
    fn load_da_file(&mut self) -> Result<Vec<u8>, String> {
        if let Some(ref data) = self.da_file_data {
            trace!("[DA_CACHE] 复用缓存的 DA 文件数据 ({} 字节)", data.len());
            return Ok(data.clone());
        }
        let (mut file, _path) = Self::open_da_file()?;
        let mut da_data = Vec::new();
        file.read_to_end(&mut da_data)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;
        trace!("[DA_CACHE] 读取 DA 文件并缓存 ({} 字节)", da_data.len());
        self.da_file_data = Some(da_data.clone());
        Ok(da_data)
    }

    /// 上传第一阶段 DA
    /// 对照 Python xflash_lib.py:upload_da1
    pub fn upload_da1(&mut self) -> Result<bool, String> {
        trace!("上传 XFlash 阶段 1...");

        let da_data = self.load_da_file()?;
        let (_magic, regions, _is_v6) = parse_da_header(&da_data, DA_HW_CODE_MT6768)?;

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

        // jump_da 内部已有 flush_input_poll，无需重复刷新
        self.preloader.jump_da(da1_address)?;

        // Python: sync = self.usbread(1) 等待 0xC0 — 已有 5s 超时
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        let mut sync = [0u8; 1];
        self.preloader.device.read(&mut sync)?;
        self.preloader.device.set_timeout(orig_timeout);
        if sync[0] != 0xC0 {
            return Err(format!("Error DA 同步: 0x{:02X}", sync[0]));
        }
        trace!("DA 同步 OK (0xC0)");

        // Python: self.sync() 发送 XFlash SYNC_SIGNAL
        self.xflash_sync()?;

        // Python: self.setup_env()
        self.setup_env()?;

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

        let da_data = self.load_da_file()?;
        let (_magic, regions, _is_v6) = parse_da_header(&da_data, DA_HW_CODE_MT6768)?;

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

        if !self.boot_to(da2_address, &da2_data, true, 0.5)? {
            return Err("上传 Stage2 失败".to_string());
        }

        trace!("Stage2 上传成功");
        Ok(true)
    }
}
