//! DA/XML V6 上传和初始化流程
//!
//! 对齐 Python mtkclient Library/DA/xml/xml_lib.py 中的 upload_da1/setup_env/setup_hw_init

use log::{info, debug};

use crate::da::xml::protocol::{cmd, send_xml_cmd, send_xml_data_blocks};
use crate::da::xml::DAXML;
use crate::da::loader::header::parse_da_header;

impl<'a> DAXML<'a> {
    /// 完整 DA 加载流程（对齐 XFlash 的 da_load）
    ///
    /// 流程：
    /// 1. CONNECT
    /// 2. UPLOAD-DA (DA1)
    /// 3. SETUP-ENV
    /// 4. SETUP-HW-INIT
    /// 5. UPLOAD-DA (DA2)
    pub fn load_da(&mut self, da_path: Option<&str>) -> Result<(), String> {
        let path = da_path.or(self.da_path.as_deref())
            .ok_or("未指定 DA 文件路径")?;

        info!("[XML DA] 加载 DA 文件: {}", path);

        // 读取 DA 文件
        let da_data = std::fs::read(path)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;

        // 获取当前芯片的 hw_code 用于匹配 DA
        let hw_code = self.preloader.chip.as_ref().map(|c| c.hw_code).unwrap_or(0);

        // 解析 DA 文件头
        let (magic, regions, is_v6) = parse_da_header(&da_data, hw_code)
            .map_err(|e| format!("解析 DA 文件失败: {}", e))?;

        info!("[XML DA] DA magic=0x{:08X}, regions={}, v6={}", magic, regions.len(), is_v6);

        if regions.len() < 2 {
            return Err(format!("DA 文件 region 数量不足: {}", regions.len()));
        }

        // Step 1: CONNECT
        self.xml_connect()?;

        // Step 2: UPLOAD-DA1
        self.xml_upload_da1(&da_data, &regions[0])?;

        // Step 3: SETUP-ENV
        self.xml_setup_env()?;

        // Step 4: SETUP-HW-INIT
        self.xml_setup_hw_init()?;

        // Step 5: UPLOAD-DA2
        self.xml_upload_da2(&da_data, &regions[1])?;

        self.initialized = true;
        info!("[XML DA] DA 加载完成");
        Ok(())
    }

    /// CONNECT 命令：建立 XML DA 会话
    fn xml_connect(&mut self) -> Result<(), String> {
        debug!("[XML DA] 发送 CONNECT...");
        let resp = send_xml_cmd(self.preloader, cmd::CONNECT, &[])?;
        if !resp.is_ok {
            return Err(format!("CONNECT 失败: {:?}", resp));
        }
        debug!("[XML DA] CONNECT 成功");
        Ok(())
    }

    /// UPLOAD-DA1：上传第一阶段 DA
    fn xml_upload_da1(&mut self, da_data: &[u8], region: &crate::da::loader::header::DaRegion) -> Result<(), String> {
        let da1_offset = region.buf_offset as usize;
        let da1_len = region.len as usize;
        let da1_addr = region.start_addr as u32;

        if da_data.len() < da1_offset + da1_len {
            return Err(format!(
                "DA 文件长度不足: 需要 {} 字节，实际 {} 字节",
                da1_offset + da1_len,
                da_data.len()
            ));
        }
        let da1_data = &da_data[da1_offset..da1_offset + da1_len];

        info!("[XML DA] 上传 DA1: {} 字节 @ 0x{:X}", da1_len, da1_addr);

        // 发送 UPLOAD-DA 命令
        let params = vec![
            ("address".to_string(), format!("0x{:X}", da1_addr)),
            ("length".to_string(), format!("{}", da1_len)),
        ];
        let resp = send_xml_cmd(self.preloader, cmd::UPLOAD_DA, &params)?;
        if !resp.is_ok {
            return Err(format!("UPLOAD-DA1 命令失败: {:?}", resp));
        }

        // 发送 DA1 数据（分块传输）
        send_xml_data_blocks(self.preloader, da1_data)?;

        // 读取上传确认
        let resp = send_xml_cmd(self.preloader, cmd::UPLOAD_DA, &[])?;
        if !resp.is_ok {
            return Err(format!("UPLOAD-DA1 确认失败: {:?}", resp));
        }

        info!("[XML DA] DA1 上传成功");
        Ok(())
    }

    /// SETUP-ENV：配置 DA 运行环境
    fn xml_setup_env(&mut self) -> Result<(), String> {
        debug!("[XML DA] 发送 SETUP-ENV...");
        let resp = send_xml_cmd(self.preloader, cmd::SETUP_ENV, &[])?;
        if !resp.is_ok {
            return Err(format!("SETUP-ENV 失败: {:?}", resp));
        }
        debug!("[XML DA] SETUP-ENV 成功");
        Ok(())
    }

    /// SETUP-HW-INIT：硬件初始化
    fn xml_setup_hw_init(&mut self) -> Result<(), String> {
        debug!("[XML DA] 发送 SETUP-HW-INIT...");
        let resp = send_xml_cmd(self.preloader, cmd::SETUP_HW_INIT, &[])?;
        if !resp.is_ok {
            return Err(format!("SETUP-HW-INIT 失败: {:?}", resp));
        }
        debug!("[XML DA] SETUP-HW-INIT 成功");
        Ok(())
    }

    /// UPLOAD-DA2：上传第二阶段 DA
    fn xml_upload_da2(&mut self, da_data: &[u8], region: &crate::da::loader::header::DaRegion) -> Result<(), String> {
        let da2_offset = region.buf_offset as usize;
        let da2_len = region.len as usize;
        let da2_addr = region.start_addr as u32;

        if da_data.len() < da2_offset + da2_len {
            return Err(format!(
                "DA 文件长度不足以包含 DA2: 需要 {} 字节，实际 {} 字节",
                da2_offset + da2_len,
                da_data.len()
            ));
        }
        self.da2_data = da_data[da2_offset..da2_offset + da2_len].to_vec();
        self.da2_base_addr = da2_addr as u64;

        info!(
            "[XML DA] 上传 DA2: {} 字节 @ 0x{:X}",
            da2_len,
            da2_addr
        );

        let params = vec![
            ("address".to_string(), format!("0x{:X}", da2_addr)),
            ("length".to_string(), format!("{}", da2_len)),
        ];
        let resp = send_xml_cmd(self.preloader, cmd::UPLOAD_DA, &params)?;
        if !resp.is_ok {
            return Err(format!("UPLOAD-DA2 命令失败: {:?}", resp));
        }

        // 发送 DA2 数据
        send_xml_data_blocks(self.preloader, &self.da2_data.clone())?;

        // 读取上传确认
        let resp = send_xml_cmd(self.preloader, cmd::UPLOAD_DA, &[])?;
        if !resp.is_ok {
            return Err(format!("UPLOAD-DA2 确认失败: {:?}", resp));
        }

        info!("[XML DA] DA2 上传成功");
        Ok(())
    }

    /// 获取设备信息（EMMC + Chip ID）
    pub fn xml_get_device_info(&mut self) -> Result<(crate::da::EmmcInfo, Vec<u8>), String> {
        // GET-EMMC-INFO
        let emmc_resp = send_xml_cmd(self.preloader, cmd::GET_EMMC_INFO, &[])?;
        if !emmc_resp.is_ok {
            return Err(format!("GET-EMMC-INFO 失败: {:?}", emmc_resp));
        }

        let emmc_info = crate::da::EmmcInfo {
            boot1_size: emmc_resp.get("boot1_size").unwrap_or("0").parse().unwrap_or(0),
            boot2_size: emmc_resp.get("boot2_size").unwrap_or("0").parse().unwrap_or(0),
            rpmb_size: emmc_resp.get("rpmb_size").unwrap_or("0").parse().unwrap_or(0),
            user_size: emmc_resp.get("user_size").unwrap_or("0").parse().unwrap_or(0),
            block_size: emmc_resp.get("block_size").unwrap_or("512").parse().unwrap_or(512),
            emmc_type: emmc_resp.get("type").unwrap_or("unknown").to_string(),
            cid: Vec::new(),
        };

        // GET-CHIP-ID
        let chip_resp = send_xml_cmd(self.preloader, cmd::GET_CHIP_ID, &[])?;
        let hw_code = chip_resp.get("hw_code").unwrap_or("0");
        let hw_subcode = chip_resp.get("hw_subcode").unwrap_or("0");
        debug!("[XML DA] Chip: hw_code={}, hw_subcode={}", hw_code, hw_subcode);

        Ok((emmc_info, Vec::new()))
    }
}
