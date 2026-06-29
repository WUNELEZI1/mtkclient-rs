//! SecCfg V3 结构（44 字节头 + SEJ 加密段 + 4 字节 endflag）
//!
//! 加密类型：SW / V2 / V3 / V4 四种 hwtype，对应不同 SEJ 加解密算法。

use log::info;

use crate::安全::sej::{
    sej_sec_cfg_hw_decrypt, sej_sec_cfg_hw_encrypt, sej_sec_cfg_hw_v3_decrypt,
    sej_sec_cfg_hw_v3_encrypt, sej_sec_cfg_sw_decrypt, sej_sec_cfg_sw_encrypt,
};

#[allow(dead_code)] // 预留：V3 版 SecCfg 解析，兼容旧版设备
pub(crate) struct SecCfgV3 {
    pub(crate) info_header: [u8; 16],
    pub(crate) magic: u32,
    pub(crate) seccfg_ver: u32,
    pub(crate) seccfg_size: u32,
    pub(crate) seccfg_enc_offset: u32,
    pub(crate) seccfg_enc_len: u32,
    pub(crate) sw_sec_lock_try: u8,
    pub(crate) sw_sec_lock_done: u8,
    pub(crate) page_size: u16,
    pub(crate) page_count: u32,
    pub(crate) seccfg_attr: u32,
    pub(crate) seccfg_status: u32,
    pub(crate) endflag: u32,
    pub(crate) hwtype: String,
    pub(crate) imginfo: Vec<[u8; 0x68]>,
    pub(crate) seccfg_ext: Vec<u8>,
}

impl SecCfgV3 {
    pub(crate) const MAGIC: u32 = 0x4D4D4D4D;
    pub(crate) const ENDFLAG: u32 = 0x45454545;
    pub(crate) const ATTR_UNLOCK: u32 = 0x44444444;
    pub(crate) const ATTR_DEFAULT: u32 = 0x33333333;

    pub(crate) fn parse(data: &[u8]) -> Result<SecCfgV3, String> {
        if data.len() < 44 {
            return Err("seccfg V3 数据太小".to_string());
        }

        let info_header: [u8; 16] = data[0..16].try_into().unwrap();
        if &info_header != b"AND_SECCFG_v\x00\x00\x00\x00" {
            return Err("非 V3 seccfg 结构".to_string());
        }

        let magic = u32::from_le_bytes(data[16..20].try_into().unwrap());
        let ver = u32::from_le_bytes(data[20..24].try_into().unwrap());
        let size = u32::from_le_bytes(data[24..28].try_into().unwrap());
        let enc_off = u32::from_le_bytes(data[28..32].try_into().unwrap());
        let enc_len = u32::from_le_bytes(data[32..36].try_into().unwrap());
        let sw_try = data[36];
        let sw_done = data[37];
        let pg_size = u16::from_le_bytes(data[38..40].try_into().unwrap());
        let pg_count = u32::from_le_bytes(data[40..44].try_into().unwrap());

        if magic != Self::MAGIC {
            return Err("seccfg V3 magic 不匹配".to_string());
        }

        let enc_data_start = size as usize - 0x2C - 4;
        let enc_data_end = size as usize - 4;
        if enc_data_start >= data.len() || enc_data_end > data.len() {
            return Err("seccfg V3 加密数据段超出范围".to_string());
        }
        let enc_data = &data[enc_data_start..enc_data_end];

        let endflag = u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap());
        if endflag != Self::ENDFLAG {
            return Err("seccfg V3 endflag 不匹配".to_string());
        }

        let mut hwtype = String::new();
        let mut decrypted = Vec::new();

        if let Ok(d) = sej_sec_cfg_sw_decrypt(enc_data) {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "SW".to_string();
                decrypted = d;
            }
        }

        if hwtype.is_empty()
            && let Ok(d) = sej_sec_cfg_hw_v3_decrypt(enc_data, false)
        {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "V2".to_string();
                decrypted = d;
            }
        }

        if hwtype.is_empty()
            && let Ok(d) = sej_sec_cfg_hw_decrypt(enc_data)
        {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "V3".to_string();
                decrypted = d;
            }
        }

        if hwtype.is_empty()
            && let Ok(d) = sej_sec_cfg_hw_v3_decrypt(enc_data, true)
        {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "V4".to_string();
                decrypted = d;
            }
        }

        if hwtype.is_empty() {
            return Err("无法识别 seccfg V3 加密类型".to_string());
        }

        if decrypted.len() < 0x2C {
            return Err("seccfg V3 解密数据太小".to_string());
        }

        let mut imginfo = Vec::new();
        for i in 0..20 {
            let offset = i * 0x68;
            if offset + 0x68 <= decrypted.len() {
                let mut entry = [0u8; 0x68];
                entry.copy_from_slice(&decrypted[offset..offset + 0x68]);
                imginfo.push(entry);
            }
        }

        let siu_offset = 20 * 0x68;
        let _siu_status = if siu_offset + 4 <= decrypted.len() {
            u32::from_le_bytes(decrypted[siu_offset..siu_offset + 4].try_into().unwrap())
        } else {
            0
        };

        let status_offset = siu_offset + 4;
        let seccfg_status = if status_offset + 4 <= decrypted.len() {
            u32::from_le_bytes(
                decrypted[status_offset..status_offset + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            0
        };

        let attr_offset = status_offset + 4;
        let seccfg_attr = if attr_offset + 4 <= decrypted.len() {
            u32::from_le_bytes(decrypted[attr_offset..attr_offset + 4].try_into().unwrap())
        } else {
            0
        };

        info!(
            "seccfg V3 解析成功: hwtype={}, attr=0x{:08X}, status=0x{:08X}",
            hwtype, seccfg_attr, seccfg_status
        );

        Ok(SecCfgV3 {
            info_header,
            magic,
            seccfg_ver: ver,
            seccfg_size: size,
            seccfg_enc_offset: enc_off,
            seccfg_enc_len: enc_len,
            sw_sec_lock_try: sw_try,
            sw_sec_lock_done: sw_done,
            page_size: pg_size,
            page_count: pg_count,
            seccfg_attr,
            seccfg_status,
            endflag,
            hwtype,
            imginfo,
            seccfg_ext: Vec::new(),
        })
    }

    pub(crate) fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let new_attr = if lockflag == "unlock" {
            if self.seccfg_attr != Self::ATTR_DEFAULT
                && self.seccfg_attr != 0x6003
                && self.seccfg_attr != 0x6002
                && self.seccfg_attr != 0x6001
                && self.seccfg_attr != 0x6000
            {
                return Err("无法找到解锁状态".to_string());
            }
            Self::ATTR_UNLOCK
        } else if lockflag == "lock" {
            if self.seccfg_attr != Self::ATTR_UNLOCK {
                return Err("无法找到上锁状态".to_string());
            }
            Self::ATTR_DEFAULT
        } else {
            return Err("无效 lockflag".to_string());
        };

        let new_enc_len: u32 = if lockflag == "unlock" {
            0x07F20000
        } else {
            0x01000000
        };

        let mut inner = Vec::new();
        for img in &self.imginfo {
            inner.extend_from_slice(img);
        }
        inner.extend_from_slice(&0u32.to_le_bytes());
        inner.extend_from_slice(&self.seccfg_status.to_le_bytes());
        inner.extend_from_slice(&new_attr.to_le_bytes());
        inner.extend_from_slice(&self.seccfg_ext);

        let enc_data = match self.hwtype.as_str() {
            "SW" => sej_sec_cfg_sw_encrypt(&inner)?,
            "V2" => sej_sec_cfg_hw_encrypt(&inner)?,
            "V3" => sej_sec_cfg_hw_v3_encrypt(&inner, false)?,
            "V4" => sej_sec_cfg_hw_v3_encrypt(&inner, true)?,
            _ => return Err(format!("不支持的 hwtype: {}", self.hwtype)),
        };

        let mut result = Vec::new();
        result.extend_from_slice(&self.info_header);
        result.extend_from_slice(&self.magic.to_le_bytes());
        result.extend_from_slice(&self.seccfg_ver.to_le_bytes());
        result.extend_from_slice(&self.seccfg_size.to_le_bytes());
        result.extend_from_slice(&self.seccfg_enc_offset.to_le_bytes());
        result.extend_from_slice(&new_enc_len.to_le_bytes());
        result.push(self.sw_sec_lock_try);
        result.push(self.sw_sec_lock_done);
        result.extend_from_slice(&self.page_size.to_le_bytes());
        result.extend_from_slice(&self.page_count.to_le_bytes());
        result.extend_from_slice(&enc_data);
        result.extend_from_slice(&self.endflag.to_le_bytes());

        while result.len() % 0x200 != 0 {
            result.push(0);
        }
        while result.len() < partition_size {
            result.push(0);
        }

        info!(
            "seccfg V3 修改成功: attr=0x{:08X} -> 0x{:08X}",
            self.seccfg_attr, new_attr
        );
        Ok(result)
    }
}
