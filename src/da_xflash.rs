use crate::preloader::Preloader;
use log::{debug, info, warn};
use std::fs::File;
use std::io::Read;
use std::thread::sleep;
use std::time::Duration;

// DA 文件 region 结构
#[derive(Debug, Clone)]
struct DaRegion {
    buf_offset: u32, // 在文件中的偏移
    len: u32,        // 大小
    start_addr: u32, // 加载地址
    sig_len: u32,    // 签名长度
}

/// 从 DA header 数据中解析 region 信息
fn parse_da_regions(header: &[u8], region_start: usize, entry_region_count: u16) -> Vec<DaRegion> {
    let mut regions = Vec::new();

    for j in 0..entry_region_count as usize {
        let region_offset = region_start + j * 20;
        if region_offset + 20 > header.len() {
            break;
        }

        let buf_offset = u32::from_le_bytes([
            header[region_offset],
            header[region_offset + 1],
            header[region_offset + 2],
            header[region_offset + 3],
        ]);
        let len = u32::from_le_bytes([
            header[region_offset + 4],
            header[region_offset + 5],
            header[region_offset + 6],
            header[region_offset + 7],
        ]);
        let start_addr = u32::from_le_bytes([
            header[region_offset + 8],
            header[region_offset + 9],
            header[region_offset + 10],
            header[region_offset + 11],
        ]);
        let sig_len = u32::from_le_bytes([
            header[region_offset + 16],
            header[region_offset + 17],
            header[region_offset + 18],
            header[region_offset + 19],
        ]);

        regions.push(DaRegion {
            buf_offset,
            len,
            start_addr,
            sig_len,
        });
    }

    regions
}

// 解析 MTK AllInOne DA 文件
fn parse_da_header(
    da_data: &[u8],
    target_hw_code: u16,
) -> Result<(u32, Vec<DaRegion>, bool), String> {
    if da_data.len() < 0x6C {
        return Err("DA 文件太小，无法解析头".to_string());
    }

    // 检查是否是 MTK AllInOne DA 格式
    let header = &da_data[0..0x68];
    let is_allinone = header.contains(&0x4D) && header.contains(&0x54) && header.contains(&0x4B);

    if is_allinone {
        // 读取 DA 数量
        let count_da =
            u32::from_le_bytes([da_data[0x68], da_data[0x69], da_data[0x6A], da_data[0x6B]]);

        // 检查是否是 v6 格式
        let is_v6 = header.windows(8).any(|window| window == b"MTK_DA_v6");

        // 检查是否是旧版加载器
        let old_ldr: bool;
        let offset: usize;

        if da_data.len() > 0x6C + 0xD8 + 1 {
            let marker = [da_data[0x6C + 0xD8], da_data[0x6C + 0xD8 + 1]];
            if marker == [0xDA, 0xDA] {
                offset = 0xD8;
                old_ldr = true;
            } else {
                offset = 0xDC;
                old_ldr = false;
            }
        } else {
            offset = 0xDC;
            old_ldr = false;
        }

        // 查找适合目标 HW Code 的 DA
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];

            // 读取 DA 信息
            let magic = u16::from_le_bytes([da_header[0], da_header[1]]);
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);
            let _hw_sub_code = u16::from_le_bytes([da_header[4], da_header[5]]);
            let _hw_version = u16::from_le_bytes([da_header[6], da_header[7]]);

            // 新版加载器有 sw_version + reserved1 (4 字节)
            let _sw_version: u16;
            let _page_size: u16;
            let _entry_region_index: u16;
            let entry_region_count: u16;
            let region_start: usize;

            if old_ldr {
                _sw_version = 0;
                _page_size = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved = u16::from_le_bytes([da_header[10], da_header[11]]);
                _entry_region_index = u16::from_le_bytes([da_header[12], da_header[13]]);
                entry_region_count = u16::from_le_bytes([da_header[14], da_header[15]]);
                region_start = 16;
            } else {
                _sw_version = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved1 = u16::from_le_bytes([da_header[10], da_header[11]]);
                _page_size = u16::from_le_bytes([da_header[12], da_header[13]]);
                let _reserved3 = u16::from_le_bytes([da_header[14], da_header[15]]);
                _entry_region_index = u16::from_le_bytes([da_header[16], da_header[17]]);
                entry_region_count = u16::from_le_bytes([da_header[18], da_header[19]]);
                region_start = 20;
            }

            // 跳过不匹配的 DA，静默处理
            if hw_code != target_hw_code {
                continue;
            }

            info!("找到匹配的 DA {}，HW Code: 0x{:04X}", i, hw_code);

            let regions = parse_da_regions(da_header, region_start, entry_region_count);

            return Ok((magic as u32, regions, is_v6));
        }

        // 如果没有找到匹配的 HW Code，返回第一个有效的 DA（hw_code 不为 0）
        warn!(
            "未找到 HW Code 0x{:04X} 的 DA，尝试使用第一个有效的 DA",
            target_hw_code
        );
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);

            if hw_code != 0 {
                let region_start = da_offset + 16;
                let entry_region_count = if offset >= 16 {
                    u16::from_le_bytes([da_header[14], da_header[15]])
                } else {
                    0
                };

                let mut regions = parse_da_regions(da_header, region_start, entry_region_count);

                // 如果没有读取到 regions，使用默认的 Stage1 和 Stage2 region
                if regions.is_empty() {
                    warn!("备选 DA 没有 region 信息，使用默认值");
                    // 默认 Stage1 region
                    regions.push(DaRegion {
                        buf_offset: 0x376C,
                        len: 0x270,
                        start_addr: 0x200000,
                        sig_len: 0x0,
                    });
                    // 默认 Stage2 region
                    regions.push(DaRegion {
                        buf_offset: 0x39E4,
                        len: 0xE660,
                        start_addr: 0x80000000,
                        sig_len: 0x100,
                    });
                }

                info!("使用 DA {} (HW Code: 0x{:04X}) 作为备选", i, hw_code);
                return Ok((0, regions, is_v6));
            }
        }

        Err("在 AllInOne DA 中未找到有效的 DA 配置".to_string())
    } else {
        // 传统 DA 格式解析
        let magic = u32::from_le_bytes([da_data[0], da_data[1], da_data[2], da_data[3]]);
        let hw_code = u16::from_le_bytes([da_data[4], da_data[5]]);
        let entry_region_count = u16::from_le_bytes([da_data[16], da_data[17]]);

        if hw_code != target_hw_code {
            warn!(
                "DA HW Code (0x{:04X}) 与目标 (0x{:04X}) 不匹配",
                hw_code, target_hw_code
            );
        }

        let is_v6 = magic == 0x5644415F;

        let regions = parse_da_regions(da_data, 0x6C, entry_region_count);

        Ok((magic, regions, is_v6))
    }
}

// XFlash 命令常量
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const CMD_SETUP_ENVIRONMENT: u32 = 0x010100;
const CMD_SETUP_HW_INIT_PARAMS: u32 = 0x010101;
const CMD_INIT_EXT_RAM: u32 = 0x01000A;
const CMD_BOOT_TO: u32 = 0x010008;
const CMD_READ_DATA: u32 = 0x010005; // XFlash 读分区命令
pub const CMD_WRITE_DATA: u32 = 0x010004; // 写入数据命令

pub const CMD_FORMAT: u32 = 0x010003; // 格式化命令
pub const SET_META_BOOT_MODE: u32 = 0x020006;

/// pack3: 生成 XFlash 参数包头 (magic(4) + data_type(4) + length(4))
pub fn pack3(magic: u32, data_type: u32, length: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..8].copy_from_slice(&data_type.to_le_bytes());
    buf[8..12].copy_from_slice(&length.to_le_bytes());
    buf
}

/// ACK 响应枚举 — 替代裸 u32 返回值
#[derive(Debug, PartialEq)]
pub(crate) enum AckResult {
    /// status == 0，设备就绪，继续传输
    Continue,
    /// status != 0，传输终止或设备报错
    Terminated(u32),
}

/// EMMC 信息结构
pub struct EmmcInfo {
    pub boot1_size: u64,
    pub boot2_size: u64,
}

/// DAXFlash 结构体，处理 XFlash 协议
pub struct DAXFlash<'a> {
    pub preloader: &'a mut Preloader,
    emi: Option<Vec<u8>>,
    emi_version: u32,
    pub(crate) da2_data: Vec<u8>,
    pub(crate) da2_base_addr: u64,
    pub daext: bool,
    pub(crate) last_gpt_data: Option<Vec<u8>>,
    pub patch_da: bool,
}

impl<'a> DAXFlash<'a> {
    pub fn new(preloader: &'a mut Preloader) -> Self {
        DAXFlash {
            preloader,
            emi: None,
            emi_version: 0,
            da2_data: Vec::new(),
            da2_base_addr: 0x40000000,
            daext: false,
            last_gpt_data: None,
            patch_da: true,
        }
    }

    /// 从 preloader 文件中提取 EMI 数据
    pub fn load_preloader_emi(&mut self, preloader_path: &str) -> Result<bool, String> {
        info!("加载 preloader 文件: {}", preloader_path);

        // 读取 preloader 文件
        let preloader_data = match std::fs::read(preloader_path) {
            Ok(data) => data,
            Err(e) => {
                warn!("无法打开 preloader 文件: {}, 继续执行", e);
                return Ok(false);
            }
        };

        info!("preloader 文件大小: {} 字节", preloader_data.len());

        // 提取 EMI 数据
        match self.extract_emi(&preloader_data) {
            Ok((version, emi_data)) => {
                let emi_len = emi_data.len();
                self.emi = Some(emi_data);
                self.emi_version = version;
                info!(
                    "成功提取 EMI 数据，版本: {}, 大小: {} 字节",
                    version, emi_len
                );
                Ok(true)
            }
            Err(e) => {
                warn!("提取 EMI 数据失败: {}, 继续执行", e);
                Ok(false)
            }
        }
    }

    /// 提取 EMI 数据的内部方法
    fn extract_emi(&self, data: &[u8]) -> Result<(u32, Vec<u8>), String> {
        // 查找标记
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        let idx = data
            .windows(marker.len())
            .position(|window| window == marker);

        let mut emi_data = data.to_vec();

        if let Some(idx) = idx {
            info!("找到 EMI 标记，偏移: 0x{:08X}", idx);
            emi_data = data[idx..].to_vec();

            // 读取 mlen 和 siglen
            if emi_data.len() >= 0x30 {
                let mlen = u32::from_le_bytes(emi_data[0x20..0x24].try_into().unwrap());
                let siglen = u32::from_le_bytes(emi_data[0x2C..0x30].try_into().unwrap());
                info!("mlen: 0x{:08X}, siglen: 0x{:08X}", mlen, siglen);

                // 截取数据
                if mlen as usize <= emi_data.len() {
                    emi_data = emi_data[..mlen as usize - siglen as usize].to_vec();
                }

                // 读取 dramsize
                if emi_data.len() >= 4 {
                    let dramsize =
                        u32::from_le_bytes(emi_data[emi_data.len() - 4..].try_into().unwrap());
                    info!("dramsize: 0x{:08X}", dramsize);

                    if dramsize == 0 && emi_data.len() >= 0x804 {
                        emi_data = emi_data[..emi_data.len() - 0x800].to_vec();
                        if emi_data.len() >= 4 {
                            let dramsize = u32::from_le_bytes(
                                emi_data[emi_data.len() - 4..].try_into().unwrap(),
                            );
                            info!("调整后 dramsize: 0x{:08X}", dramsize);
                        }
                    }

                    // 截取 EMI 数据
                    if dramsize > 0 && emi_data.len() >= (dramsize + 4) as usize {
                        emi_data = emi_data
                            [emi_data.len() - (dramsize + 4) as usize..emi_data.len() - 4]
                            .to_vec();
                    }
                }
            }
        }

        // 查找 MTK_BLOADER_INFO_v 字符串
        let bldrstring = b"MTK_BLOADER_INFO_v";
        let idx = emi_data
            .windows(bldrstring.len())
            .position(|window| window == bldrstring);

        if let Some(idx) = idx {
            info!("找到 MTK_BLOADER_INFO_v，偏移: 0x{:08X}", idx);

            // 提取版本号
            let version_str = &emi_data[idx + bldrstring.len()..idx + bldrstring.len() + 2];
            let version = String::from_utf8_lossy(version_str)
                .trim_end_matches('\0')
                .parse::<u32>()
                .unwrap_or_default();
            info!("EMI 版本: {}", version);

            // ⚠️ 关键修复：如果 MTK_BLOADER_INFO_v 在偏移 0，返回整个 emi_data
            // 对齐 Python m_extract_emi: if idx == 0 and damode == XFLASH: return ver, data
            if idx == 0 {
                debug!("MTK_BLOADER_INFO_v 在偏移 0，使用完整 EMI 数据（含 header）");
                debug!("EMI 数据大小: {} 字节", emi_data.len());
                return Ok((version, emi_data));
            }

            // 原有逻辑：只提取 MTK_BIN 后的数据
            let mtk_bin_idx = emi_data.windows(7).position(|window| window == b"MTK_BIN");
            if let Some(mtk_bin_idx) = mtk_bin_idx {
                let emi = emi_data[mtk_bin_idx + 0xC..].to_vec();
                info!("EMI 数据大小: {} 字节", emi.len());
                return Ok((version, emi));
            }
        }

        Err("未找到 EMI 数据".to_string())
    }

    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    fn xread(&mut self) -> Result<u32, String> {
        // 读取 12 字节的 XFlash 头
        let mut header = [0; 12];
        self.preloader.device.read(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let _data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            return Err(format!("XFlash 头 magic 错误: 0x{:08X}", magic));
        }

        // 读取数据
        if length > 0 {
            let mut data = vec![0; length as usize];
            self.preloader.device.read(&mut data)?;

            // 如果数据是 4 字节，返回 u32 值
            if length == 4 {
                return Ok(u32::from_le_bytes(data[0..4].try_into().unwrap()));
            }
        }

        Ok(0)
    }

    /// XFlash 同步命令
    /// Python: sync() → 只 xsend(CMD_SYNC_SIGNAL)，不读 status 也不读 response
    fn xflash_sync(&mut self) -> Result<bool, String> {
        debug!("执行 XFlash 同步命令...");

        // Python: self.sync() → self.xsend(self.Cmd.SYNC_SIGNAL)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SYNC_SIGNAL.to_le_bytes())?;

        Ok(true)
    }

    /// 设置环境
    /// Python: xsend(CMD_SETUP_ENVIRONMENT) → send_param(20字节) → status()
    pub fn setup_env(&mut self) -> Result<bool, String> {
        debug!("设置环境...");

        // xsend(CMD_SETUP_ENVIRONMENT)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SETUP_ENVIRONMENT.to_le_bytes())?;

        // send_param(20字节): da_log_level, log_channel, system_os, ufs_provision, 0x0
        let param: [u8; 20] = [
            0x00, 0x00, 0x00, 0x00, // da_log_level = 0
            0x01, 0x00, 0x00, 0x00, // log_channel = 1
            0x01, 0x00, 0x00, 0x00, // system_os = OS_LINUX = 1
            0x00, 0x00, 0x00, 0x00, // ufs_provision = 0
            0x00, 0x00, 0x00, 0x00, // 0x0
        ];
        let param_pkt = pack3(CMD_MAGIC, 0x01, 20);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("setup_env status error: 0x{:08X}", st));
        }

        debug!("环境设置成功");
        Ok(true)
    }

    /// 初始化硬件
    /// Python: xsend(CMD_SETUP_HW_INIT_PARAMS) → send_param(pack("<I", 0x0)) → status()
    pub fn setup_hw_init(&mut self) -> Result<bool, String> {
        info!("初始化硬件...");

        // xsend(CMD_SETUP_HW_INIT_PARAMS)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SETUP_HW_INIT_PARAMS.to_le_bytes())?;

        // send_param(pack("<I", 0x0)): 4字节参数 = 0x0
        let param = 0x0u32.to_le_bytes();
        let param_pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("setup_hw_init status error: 0x{:08X}", st));
        }

        debug!("硬件初始化成功");
        Ok(true)
    }

    /// 发送 EMI 数据初始化 DRAM
    /// 流程：发送 INIT_EXT_RAM → 发送 EMI 数据 → 验证状态
    pub fn send_emi(&mut self, emi: &[u8]) -> Result<bool, String> {
        debug!("发送 EMI 数据初始化 DRAM...");
        debug!(
            "[EMI DEBUG] len={}, first 64 bytes={}",
            emi.len(),
            emi[..std::cmp::min(64, emi.len())]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        // 1. xsend(INIT_EXT_RAM)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_INIT_EXT_RAM.to_le_bytes())?;

        // 2. status() - Python reads immediately, no sleep
        let st = self.status()?;
        debug!("[EMI DEBUG] INIT_EXT_RAM status=0x{:08X}", st);
        if st != 0 {
            return Err(format!("INIT_EXT_RAM status error: 0x{:08X}", st));
        }

        // 3. sleep(0.01) - Python sleeps AFTER status check
        sleep(Duration::from_millis(10));

        // 4. xsend(len(emi)) - Python sends header + length value
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader
            .device
            .write(&(emi.len() as u32).to_le_bytes())?;

        // 5. send_param([emi]) - Python sends param header + data in 512-byte chunks
        let param_pkt = pack3(CMD_MAGIC, 0x01, emi.len() as u32);
        self.preloader.device.write(&param_pkt)?;

        // Python send_param splits data into 0x200 (512) byte chunks
        let chunk_size = 0x200;
        let mut pos = 0;
        let mut remaining = emi.len();
        while remaining > 0 {
            let dsize = std::cmp::min(remaining, chunk_size);
            self.preloader.device.write(&emi[pos..pos + dsize])?;
            pos += dsize;
            remaining -= dsize;
        }

        // 6. status() - wait for EMI config complete (Python takes ~1.1s)
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        let st3 = self.status()?;
        self.preloader.device.set_timeout(orig_timeout);

        if st3 != 0 {
            return Err(format!("EMI data status error: 0x{:08X}", st3));
        }

        debug!("EMI 数据发送成功");
        Ok(true)
    }

    /// Boot 到指定地址
    /// 对齐 Python boot_to + send_data 流程
    pub(crate) fn boot_to(
        &mut self,
        addr: u32,
        da: &[u8],
        display: bool,
        timeout: f32,
    ) -> Result<bool, String> {
        if display {
            debug!("Boot 到地址: 0x{:08X}, 大小: {} 字节", addr, da.len());
        }

        // Python: self.xsend(self.Cmd.BOOT_TO)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_BOOT_TO.to_le_bytes())?;

        // Python: self.status()
        let st = self.status()?;
        debug!("[BOOT_TO DEBUG] status1=0x{:08X} (after xsend BOOT_TO)", st);
        if st != 0 {
            return Err(format!("boot_to status1 error: 0x{:08X}", st));
        }

        // Python: param = pack("<QQ", addr, len(da)) + usbwrite(pkt1) + usbwrite(param)
        let param_len: u64 = da.len() as u64;
        let mut param = Vec::with_capacity(16);
        param.extend_from_slice(&(addr as u64).to_le_bytes());
        param.extend_from_slice(&param_len.to_le_bytes());
        let pkt1 = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&pkt1)?;
        self.preloader.device.write(&param)?;

        // Python: self.send_data(da) — 发送 12 字节头 + 分块 64 字节数据
        let pkt2 = pack3(CMD_MAGIC, 0x01, da.len() as u32);
        self.preloader.device.write(&pkt2)?;

        let maxinsize = 64;
        let mut remaining = da.len();
        let mut pos = 0;
        let mut send_failed = false;

        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, maxinsize);
            let chunk = &da[pos..pos + chunk_size];

            match self.preloader.device.write(chunk) {
                Ok(_) => {
                    pos += chunk_size;
                    remaining -= chunk_size;
                }
                Err(e) => {
                    if display {
                        warn!(
                            "Stage2 数据传输中断（设备开始执行，剩余 {} 字节）: {}",
                            remaining, e
                        );
                    }
                    send_failed = true;
                    break;
                }
            }

            if pos % 0x2000 == 0 && !send_failed {
                self.preloader.device.write(&[]).ok();
                sleep(Duration::from_millis(10));
            }
        }

        if !send_failed {
            // Python send_data: 数据发送完成后直接读 status，不发送 ZLP
            let _ = self.status();
        }

        // Python: time.sleep(timeout) — 等待设备执行
        let sleep_ms = (timeout * 1000.0) as u64;
        sleep(Duration::from_millis(sleep_ms));
        debug!("[BOOT_TO DEBUG] slept {}ms, reading status2...", sleep_ms);

        // Python: try: status = self.status() except: ...
        // 设备可能已经重新枚举，status 读取失败是正常的
        match self.status() {
            Ok(st2) => {
                // Python: 接受 0x434E5953 (SYNC_SIGNAL="CYNS") 或 0x0 作为成功
                if st2 == 0x434E5953 || st2 == 0x0 {
                    if display {
                        debug!("Boot 成功");
                    }
                    Ok(true)
                } else {
                    // Python: 其他状态码只打印 error，不抛异常
                    if display {
                        warn!("boot_to 状态: 0x{:08X}", st2);
                    }
                    Ok(true) // 不返回错误，继续执行
                }
            }
            Err(_) => {
                // Python: status 读取失败时打印 error 但继续
                if display {
                    warn!("boot_to status 读取失败（设备已重新枚举）");
                }
                Ok(true)
            }
        }
    }

    /// 上传第一阶段 DA
    /// 对照 Python xflash_lib.py:upload_da1
    pub fn upload_da1(&mut self) -> Result<bool, String> {
        debug!("上传 XFlash 阶段 1...");

        let mut file =
            File::open("MTK_DA_V5.bin").map_err(|e| format!("无法打开 DA 文件: {}", e))?;
        let mut da_data = Vec::new();
        file.read_to_end(&mut da_data)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;

        let (_magic, regions, _is_v6) = parse_da_header(&da_data, 0x6768)?;

        if regions.len() < 2 {
            return Err("DA 文件格式错误，无法找到 Stage1 region".to_string());
        }

        let stage1 = &regions[1];
        let da1_buf_offset = stage1.buf_offset;
        let da1_len = stage1.len;
        let da1_address = stage1.start_addr;
        let _da1_sig_len = stage1.sig_len;

        debug!(
            "  偏移: 0x{:08X}, 大小: 0x{:08X}, 地址: 0x{:08X}",
            da1_buf_offset, da1_len, da1_address
        );

        let da1_start = da1_buf_offset as usize;
        let da1_end = da1_start + da1_len as usize;
        if da1_end > da_data.len() {
            return Err("DA 文件格式错误，Stage1 数据超出文件范围".to_string());
        }

        let mut da1_patched = da_data[da1_start..da1_end].to_vec();
        debug!("应用 DA1 patch...");
        Self::patch_da1(&mut da1_patched);

        if !self
            .preloader
            .send_da(da1_address, da1_len, 0, &da1_patched)?
        {
            return Err("发送 DA 失败".to_string());
        }

        debug!("成功上传 stage 1，跳转中...");

        self.preloader.jump_da(da1_address)?;

        // Give device time to start DA execution
        std::thread::sleep(Duration::from_millis(100));

        // Python: sync = self.usbread(1) 等待 0xC0
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
        debug!("DA 同步 OK (0xC0)");

        // Python: self.sync() 发送 XFlash SYNC_SIGNAL
        self.xflash_sync()?;

        // Python: self.setup_env()
        self.setup_env()?;

        // Python: self.setup_hw_init()
        self.setup_hw_init()?;

        // Python: res = self.xread(); if res == pack("<I", self.Cmd.SYNC_SIGNAL)
        let resp = self.xread()?;
        if resp != CMD_SYNC_SIGNAL {
            return Err(format!("Error jumping to DA: got 0x{:08X}", resp));
        }
        debug!("已接收 DA 同步信号");

        Ok(true)
    }

    /// 上传第二阶段 DA
    /// 流程：检查是否需要 EMI → 发送 EMI → 调用 boot_to 上传 Stage2 → reinit
    pub fn upload_da2(&mut self) -> Result<bool, String> {
        debug!("上传 XFlash 阶段 2...");

        let mut file =
            File::open("MTK_DA_V5.bin").map_err(|e| format!("无法打开 DA 文件: {}", e))?;
        let mut da_data = Vec::new();
        file.read_to_end(&mut da_data)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;

        let (_magic, regions, _is_v6) = parse_da_header(&da_data, 0x6768)?;

        if regions.len() < 3 {
            return Err("DA 文件格式错误，无法找到 Stage2 region".to_string());
        }

        let stage2 = &regions[2];
        let da2_buf_offset = stage2.buf_offset;
        let da2_len = stage2.len;
        let da2_address = stage2.start_addr;
        let da2_sig_len = stage2.sig_len;

        debug!(
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
            debug!(
                "  DA2 原始大小: 0x{:X} ({}) 字节",
                da2_size_before, da2_size_before
            );
            debug!(
                "  DA2 截断后大小: 0x{:X} ({}) 字节 (已截断 0x{:X} 字节签名)",
                da2_data.len(),
                da2_data.len(),
                sig_len
            );
        }

        debug!("应用 DA2 patch...");
        Self::patch_da2(&mut da2_data);

        self.da2_data = da2_data.clone();
        self.da2_base_addr = da2_address as u64;

        if !self.boot_to(da2_address, &da2_data, true, 0.5)? {
            return Err("上传 Stage2 失败".to_string());
        }

        debug!("Stage2 上传成功");
        Ok(true)
    }

    /// 发送 devctrl 命令
    /// Python: 任何阶段失败都返回 b""，不抛异常
    pub(crate) fn send_devctrl(
        &mut self,
        cmd: u32,
        param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        // xsend(Cmd.DEVICE_CTRL) — DEVICE_CTRL = 0x010009
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            if st != 0xC0010004 {
                warn!("send_devctrl DEVICE_CTRL 阶段1 状态: 0x{:08X}", st);
            }
            return Ok(vec![]);
        }

        // xsend(cmd)
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;

        let st2 = self.status()?;
        if st2 != 0 {
            if st2 != 0xC0010004 {
                warn!("send_devctrl(0x{:06X}) 状态: 0x{:08X}", cmd, st2);
            }
            return Ok(vec![]);
        }

        if let Some(p) = param {
            let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
            self.preloader.device.write(&pkt3)?;
            self.preloader.device.write(p)?;
            let st3 = self.status()?;
            if st3 != 0 {
                warn!("send_devctrl param 状态: 0x{:08X}", st3);
                return Ok(vec![]);
            }
        } else {
            let resp = self.xread_data()?;
            debug!(
                "[send_devctrl] cmd=0x{:06X} xread returned {} bytes",
                cmd,
                resp.len()
            );
            return Ok(resp);
        }

        Ok(vec![])
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("status magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut tmp = vec![0u8; length as usize];
            self.preloader.device.read(&mut tmp)?;
            if length == 4 {
                let val = u32::from_le_bytes(tmp[..4].try_into().unwrap());
                // Python special case: if status == 0xFEEEEEEF, return 0
                if val == 0xFEEEEEEF {
                    return Ok(0);
                }
                return Ok(val);
            } else if length == 2 {
                return Ok(u16::from_le_bytes(tmp[..2].try_into().unwrap()) as u32);
            }
        }
        Ok(0)
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("xread magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut data = vec![0u8; length as usize];
            self.preloader.device.read(&mut data)?;
            Ok(data)
        } else {
            Ok(vec![])
        }
    }

    /// 获取连接代理（brom 或 preloader）
    /// Python: 返回 b"" 或 None 时视为失败
    fn get_connection_agent(&mut self) -> Result<String, String> {
        let data = self.send_devctrl(0x010102, None)?; // GET_CONNECTION_AGENT
        if data.is_empty() {
            return Err("get_connection_agent returned empty".to_string());
        }
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// 设置重置键
    fn set_reset_key(&mut self, key: u32) -> Result<(), String> {
        self.send_devctrl(0x010103, Some(&key.to_le_bytes()))?;
        Ok(())
    }

    /// 设置校验级别
    fn set_checksum_level(&mut self, level: u32) -> Result<(), String> {
        self.send_devctrl(0x010104, Some(&level.to_le_bytes()))?;
        Ok(())
    }

    /// 获取过期日期
    fn get_expire_date(&mut self) -> Result<Vec<u8>, String> {
        let data = self.send_devctrl(0x010105, None)?;
        if data.is_empty() {
            return Err("get_expire_date returned empty".to_string());
        }
        Ok(data)
    }

    /// 获取 SLA 状态
    fn get_sla_status(&mut self) -> Result<u32, String> {
        let data = self.send_devctrl(0x01010E, None)?; // SLA_ENABLED_STATUS
        if data.len() >= 4 {
            Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
        } else {
            Err("sla_status empty".to_string())
        }
    }

    /// 重新初始化（获取 EMMC/芯片信息等）
    pub(crate) fn reinit(&mut self) -> Result<(), String> {
        // GET_RAM_INFO
        match self.send_devctrl(0x010107, None) {
            Ok(data) if data.len() >= 24 => {
                let sram_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let dram_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
                info!(
                    "  SRAM 类型: 0x{:08X}, DRAM 大小: 0x{:08X}",
                    sram_type, dram_size
                );
            }
            _ => {}
        }

        // GET_CHIP_ID
        match self.send_devctrl(0x010106, None) {
            Ok(data) if data.len() >= 10 => {
                let hw_code = u16::from_le_bytes([data[0], data[1]]);
                info!("  芯片 HW Code: 0x{:04X}", hw_code);
            }
            _ => {}
        }

        // GET_DA_VERSION
        if let Ok(data) = self.send_devctrl(0x01010A, None) {
            let ver = String::from_utf8_lossy(&data);
            info!("  DA 版本: {}", ver);
        }

        // GET_RANDOM_ID
        if let Ok(data) = self.send_devctrl(0x01010B, None) {
            debug!("  Random ID: {:02X?}", data);
        }

        // GET_EMMC_INFO
        match self.send_devctrl(0x01010C, None) {
            Ok(data) if data.len() >= 80 => {
                let emmc_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let emmc_block_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                let user_size = u64::from_le_bytes([
                    data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
                ]);
                info!(
                    "  EMMC 类型: {}, block_size: 0x{:08X}, user_size: 0x{:016X}",
                    emmc_type, emmc_block_size, user_size
                );
            }
            _ => {}
        }

        Ok(())
    }

    /// 上传 DA（完整流程，对齐 Python upload_da）
    pub fn upload_da(&mut self) -> Result<bool, String> {
        debug!("开始 DA 加载流程...");

        if !self.upload_da1()? {
            return Err("Stage1 上传失败".to_string());
        }

        match self.get_expire_date() {
            Ok(d) if !d.is_empty() => debug!("  过期日期: {:02X?}", d),
            Err(e) => warn!("get_expire_date 失败 (可能不支持): {}", e),
            _ => {}
        }

        if let Err(e) = self.set_reset_key(0x68) {
            warn!("set_reset_key 失败 (可能不支持): {}", e);
        }

        if let Err(e) = self.set_checksum_level(0x0) {
            warn!("set_checksum_level 失败 (可能不支持): {}", e);
        }

        let conn_agent = match self.get_connection_agent() {
            Ok(agent) => agent,
            Err(e) => {
                warn!("get_connection_agent 失败: {}", e);
                "brom".to_string()
            }
        };
        debug!("  连接代理: {}", conn_agent);

        if conn_agent == "brom" {
            if let Some(emi_data) = self.emi.clone() {
                debug!("发送 EMI 数据...");
                self.send_emi(&emi_data)?;
            } else {
                warn!("未找到 EMI 数据，跳过发送");
            }
        }

        if !self.upload_da2()? {
            return Err("Stage2 上传失败".to_string());
        }

        match self.get_sla_status() {
            Ok(sla) => {
                if sla != 0 {
                    debug!("  DA SLA 已启用: 0x{:08X}", sla);
                } else {
                    debug!("  DA SLA 未启用");
                }
            }
            Err(e) => warn!("get_sla_status 失败: {}", e),
        }

        if let Err(e) = self.reinit() {
            warn!("reinit 失败: {}", e);
        }

        // 10. 加载 DA extensions（对齐 Python xflash_lib.py 第 1247-1258 行）
        //      Python: daextdata = self.xft.patch()
        //              if self.boot_to(addr=0x4FFF0000, da=daextdata):
        //                  ret = self.send_devctrl(XCmd.CUSTOM_ACK)
        //                  status = self.status()
        //                  if status == 0x0 and unpack("<I", ret)[0] == 0xA1A2A3A4:
        debug!("正在加载 DA extensions...");

        // 清空 USB 输入缓冲区：reinit() 后设备可能发送残留数据，
        // 如果不干净，会污染后续 BOOT_TO 命令的响应。
        {
            let mut drain_buf = [0u8; 64];
            loop {
                let orig_timeout = self.preloader.device.get_timeout();
                self.preloader.device.set_timeout(Duration::from_millis(50));
                match self.preloader.device.read(&mut drain_buf) {
                    Ok(0) | Err(_) => {
                        self.preloader.device.set_timeout(orig_timeout);
                        break;
                    }
                    Ok(n) => {
                        debug!("[DRAIN] discarded {} bytes", n);
                    }
                }
                self.preloader.device.set_timeout(orig_timeout);
            }
        }

        if self.patch_da
            && let Some(ext_data) = self.generate_da_extensions()
        {
            match self.boot_to(0x4FFF0000, &ext_data, true, 0.5) {
                Ok(_) => {
                    // Python 第 1251 行：boot_to 成功后立刻发送 CUSTOM_ACK
                    // 给 extensions 一点初始化时间
                    sleep(Duration::from_millis(100));
                    if let Ok(ack) = self.send_devctrl(0x0F0000, None) {
                        // Python 第 1252 行：send_devctrl 后还要读一次 status
                        let status = self.status();
                        let status_ok = match status {
                            Ok(s) => s == 0,
                            Err(_) => false,
                        };
                        if ack.len() >= 4 {
                            let magic = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
                            if status_ok && magic == 0xA1A2A3A4 {
                                // Python 第 1256 行：CUSTOM_ACK 成功后立即调用 custom_set_storage
                                // CUSTOM_SET_STORAGE = 0x0F0005，参数：0=eMMC, 1=UFS
                                if self
                                    .send_devctrl(0x0F0005, Some(&0u32.to_le_bytes()))
                                    .is_ok()
                                {
                                    info!("DA Extensions 加载成功，存储类型已设置为 eMMC");
                                    self.daext = true;
                                } else {
                                    warn!("custom_set_storage 失败，extensions 功能可能受限");
                                    self.daext = true;
                                }
                            } else {
                                warn!(
                                    "DA extensions CUSTOM_ACK 验证失败 (status={:?}, magic=0x{:08X})",
                                    status, magic
                                );
                            }
                        } else {
                            warn!("DA extensions CUSTOM_ACK 响应为空 (len={})", ack.len());
                        }
                    }
                }
                Err(e) => {
                    warn!("boot_to(extensions) 失败: {}", e);
                }
            }
        }
        if !self.daext {
            warn!("DA extensions 未启用");
        }

        info!("DA 加载完成");
        Ok(true)
    }

    /// 获取 EMMC 信息（Boot1/Boot2 大小）
    /// 对齐 Python: send_devctrl(0x01010C, None) → get_emmc_info
    pub fn get_emmc_info(&mut self) -> Result<EmmcInfo, String> {
        let data = self.send_devctrl(0x01010C, None)?;
        if data.len() < 8 {
            return Err("EMMC info 数据太短".to_string());
        }
        let boot1_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as u64;
        let boot2_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
        Ok(EmmcInfo {
            boot1_size,
            boot2_size,
        })
    }

    /// 读取 flash 数据，返回原始字节
    /// 对齐 Python xflash_lib.py:879-891 (filename="" 分支):
    ///   get_packet_length → cmd_read_data → xread 循环 (header+data) → ack
    /// 注意：filename="" 分支没有 readflash_final 包，设备不会发送 final
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        // 1. get_packet_length (send_devctrl 0x040007 + status)
        // Python: get_packet_length() → send_devctrl → if resp != "": status()
        let _ = self.send_devctrl(0x040007, None);
        let _ = self.status();

        // 2. cmd_read_data: xsend(CMD_READ_DATA) → status → send_param → status
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_READ_DATA.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // send_param: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        // 这里的 size 保持调用方传入的“实际分区大小”，不要改成请求读取长度
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = 1 (eMMC)
        param.extend_from_slice(&8u32.to_le_bytes()); // parttype = 8 (USER)
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("send_param status=0x{:08X}", st2));
        }

        // 3. 数据读取循环 — 对齐 Python xflash_lib.py:879-891 (filename="" 分支)
        let mut buffer = Vec::new();
        let mut remaining = size as usize;

        while remaining > 0 {
            // 读 12 字节头
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    // ZLP 或空响应 — 设备无更多数据，正常结束
                    debug!("[readflash_data] ZLP on header read, ending loop");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    debug!(
                        "[readflash_data] read header error (end of transfer): {}",
                        e
                    );
                    break;
                }
            }

            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

            if magic != CMD_MAGIC {
                // 读到非预期数据，可能是残留状态包，容错退出
                debug!(
                    "[readflash_data] bad magic: 0x{:08X} at offset {}, ending loop",
                    magic,
                    buffer.len()
                );
                break;
            }

            // 读数据
            let mut data = vec![0u8; slength as usize];
            if slength > 0
                && let Err(e) = self.preloader.device.read_exact(&mut data)
            {
                debug!("[readflash_data] read data error: {}", e);
                break;
            }

            // 追加数据（包括心跳包的 4 字节零值）
            buffer.extend_from_slice(&data);
            remaining = remaining.saturating_sub(data.len());

            // 发送 ACK（只发不读）
            // 关键：不在此处读 status！下一个数据包的包头就是 DA 对 ACK 的响应
            // 如果读 status，会偷吃下一个数据包
            if let Err(e) = self.send_ack() {
                debug!("[readflash_data] send_ack failed: {}", e);
                break;
            }
        }

        debug!("[readflash_data] total read {} bytes", buffer.len());
        Ok(buffer)
    }

    /// 发送 ACK 并读取设备响应
    /// 写入 12B header(CMD_MAGIC + 0x01 + 4) + 4B 零值 → 读取 status
    /// 返回 AckResult::Continue（可继续）或 AckResult::Terminated（终止）
    pub(crate) fn ack(&mut self) -> AckResult {
        if let Err(e) = self.send_ack() {
            debug!("[ack] send_ack failed: {}", e);
            return AckResult::Terminated(1);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => AckResult::Terminated(n),
            Err(_) => AckResult::Terminated(3),
        }
    }

    fn send_ack(&mut self) -> Result<(), String> {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader
            .device
            .write(&hdr)
            .map_err(|e| format!("send_ack write hdr: {}", e))?;
        self.preloader
            .device
            .write(&0u32.to_le_bytes())
            .map_err(|e| format!("send_ack write data: {}", e))?;
        Ok(())
    }

    pub fn patch_vbmeta(&mut self, mode: u32) -> Result<(), String> {
        info!("修补 vbmeta，模式: {}", mode);
        Ok(())
    }

    pub fn close_device(&mut self, reset: bool) {
        if reset {
            if let Err(e) = self.preloader.jump_bl() {
                warn!("jump_bl 失败: {}", e);
            } else {
                info!("已发送 JUMP_BL 命令，设备将重启");
            }
        }
    }

    /// 通过 DA 重启设备（XFlash CMD_RESET）
    /// 对齐刷机匣：0x010007 + param(storage=1, value=0x64)
    pub fn reset_device(&mut self) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010007u32.to_le_bytes())?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("CMD_RESET status: 0x{:08X}", st));
        }

        // param: storage(4) + value(4) + zeros(20) = 28 字节
        let mut param = vec![0u8; 28];
        param[0..4].copy_from_slice(&1u32.to_le_bytes()); // storage=eMMC
        param[4..8].copy_from_slice(&100u32.to_le_bytes()); // value=0x64
        let param_pkt = pack3(CMD_MAGIC, 0x01, 28);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("CMD_RESET param status: 0x{:08X}", st2));
        }
        info!("设备已通过 DA 重启");
        Ok(())
    }

    /// 获取 EMI 数据（调试模式使用）
    pub fn get_emi_data(&self) -> Option<&Vec<u8>> {
        self.emi.as_ref()
    }

    /// 获取 DA extensions 数据（调试模式使用）
    pub fn get_extensions_data(&self) -> Option<Vec<u8>> {
        if self.da2_data.is_empty() {
            None
        } else {
            self.generate_da_extensions()
        }
    }

    /// 获取最后一次 GPT 读取的原始数据（调试模式使用）
    pub fn get_last_gpt_data(&self) -> Result<&Vec<u8>, String> {
        self.last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())
    }
}
