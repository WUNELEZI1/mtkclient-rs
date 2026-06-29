//! SecCfg V4 结构（28 字节头 + 32 字节 SHA256/AES 哈希尾部）
//!
//! - 解析：识别 SW / V2 / V3 / V4 加密类型
//! - 构建：仅修改 lock_state，不动 critical_lock_state/dm_verity

use log::info;
use sha2::{Digest, Sha256};

use crate::安全::sej::{
    sej_sec_cfg_hw_encrypt, sej_sec_cfg_hw_v3_encrypt, sej_sec_cfg_sw_decrypt,
    sej_sec_cfg_sw_encrypt,
};

/// SecCfg V4 解析和修改
#[allow(dead_code)] // 预留：unlock/lock 功能使用
pub(crate) struct SecCfgV4 {
    pub(crate) magic: u32,
    pub(crate) seccfg_ver: u32,
    pub(crate) seccfg_size: u32,
    pub(crate) lock_state: u32,
    pub(crate) critical_lock_state: u32,
    pub(crate) sboot_runtime: u32,
    pub(crate) endflag: u32,
    pub(crate) hwtype: String, // "SW", "V2", "V3", "V4"
}

impl SecCfgV4 {
    pub(crate) const MAGIC: u32 = 0x4D4D4D4D;
    pub(crate) const ENDFLAG: u32 = 0x45454545;

    /// 解析 seccfg V4 数据
    pub(crate) fn parse(data: &[u8]) -> Result<SecCfgV4, String> {
        if data.len() < 28 {
            return Err("seccfg 数据太小，无法解析 V4 头部".to_string());
        }

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let ver = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let lock = u32::from_le_bytes(data[12..16].try_into().unwrap());
        let crit_lock = u32::from_le_bytes(data[16..20].try_into().unwrap());
        let sboot = u32::from_le_bytes(data[20..24].try_into().unwrap());
        let endflag = u32::from_le_bytes(data[24..28].try_into().unwrap());

        if magic != Self::MAGIC || endflag != Self::ENDFLAG {
            return Err(format!(
                "非 V4 seccfg 结构 (magic=0x{:08X}, endflag=0x{:08X})",
                magic, endflag
            ));
        }

        if data.len() < 0x20 {
            return Err("seccfg 数据太小，无法读取哈希".to_string());
        }
        let enc_hash = data[data.len() - 0x20..data.len()].to_vec();

        let seccfg_header: [u8; 28] = [
            magic.to_le_bytes()[0],
            magic.to_le_bytes()[1],
            magic.to_le_bytes()[2],
            magic.to_le_bytes()[3],
            ver.to_le_bytes()[0],
            ver.to_le_bytes()[1],
            ver.to_le_bytes()[2],
            ver.to_le_bytes()[3],
            size.to_le_bytes()[0],
            size.to_le_bytes()[1],
            size.to_le_bytes()[2],
            size.to_le_bytes()[3],
            lock.to_le_bytes()[0],
            lock.to_le_bytes()[1],
            lock.to_le_bytes()[2],
            lock.to_le_bytes()[3],
            crit_lock.to_le_bytes()[0],
            crit_lock.to_le_bytes()[1],
            crit_lock.to_le_bytes()[2],
            crit_lock.to_le_bytes()[3],
            sboot.to_le_bytes()[0],
            sboot.to_le_bytes()[1],
            sboot.to_le_bytes()[2],
            sboot.to_le_bytes()[3],
            endflag.to_le_bytes()[0],
            endflag.to_le_bytes()[1],
            endflag.to_le_bytes()[2],
            endflag.to_le_bytes()[3],
        ];

        let expected_hash = Sha256::digest(seccfg_header);

        let mut hwtype = String::new();
        if let Ok(dec) = sej_sec_cfg_sw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            hwtype = "SW".to_string();
        }

        if hwtype.is_empty() {
            hwtype = "V4".to_string();
        }

        if hwtype.is_empty() {
            return Err("无法识别 seccfg hwtype".to_string());
        }

        info!(
            "seccfg V4 解析成功: hwtype={}, lock_state=0x{:08X}",
            hwtype, lock
        );

        Ok(SecCfgV4 {
            magic,
            seccfg_ver: ver,
            seccfg_size: size,
            lock_state: lock,
            critical_lock_state: crit_lock,
            sboot_runtime: sboot,
            endflag,
            hwtype,
        })
    }

    /// 修改 seccfg V4（lock/unlock）
    /// 对齐 C# 版正确行为：只修改 lock_state(0x0C)，不动 critical_lock_state/dm_verity(0x10)
    /// Python mtkclient 的 Bug：错误地将 offset 0x10 写为 01，导致 dm-verity corruption
    #[allow(dead_code)] // 预留：unlock-bootloader / lock-bootloader 命令调用入口
    pub(crate) fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let new_lock = if lockflag == "unlock" {
            if self.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            3u32
        } else if lockflag == "lock" {
            if self.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            1u32
        } else {
            return Err("无效 lockflag".to_string());
        };

        let seccfg_header: [u8; 28] = [
            self.magic.to_le_bytes()[0],
            self.magic.to_le_bytes()[1],
            self.magic.to_le_bytes()[2],
            self.magic.to_le_bytes()[3],
            self.seccfg_ver.to_le_bytes()[0],
            self.seccfg_ver.to_le_bytes()[1],
            self.seccfg_ver.to_le_bytes()[2],
            self.seccfg_ver.to_le_bytes()[3],
            self.seccfg_size.to_le_bytes()[0],
            self.seccfg_size.to_le_bytes()[1],
            self.seccfg_size.to_le_bytes()[2],
            self.seccfg_size.to_le_bytes()[3],
            new_lock.to_le_bytes()[0],
            new_lock.to_le_bytes()[1],
            new_lock.to_le_bytes()[2],
            new_lock.to_le_bytes()[3],
            self.critical_lock_state.to_le_bytes()[0],
            self.critical_lock_state.to_le_bytes()[1],
            self.critical_lock_state.to_le_bytes()[2],
            self.critical_lock_state.to_le_bytes()[3],
            self.sboot_runtime.to_le_bytes()[0],
            self.sboot_runtime.to_le_bytes()[1],
            self.sboot_runtime.to_le_bytes()[2],
            self.sboot_runtime.to_le_bytes()[3],
            self.endflag.to_le_bytes()[0],
            self.endflag.to_le_bytes()[1],
            self.endflag.to_le_bytes()[2],
            self.endflag.to_le_bytes()[3],
        ];

        let new_hash = Sha256::digest(seccfg_header);

        let enc_hash = match self.hwtype.as_str() {
            "SW" => sej_sec_cfg_sw_encrypt(&new_hash)?,
            "V2" => sej_sec_cfg_hw_encrypt(&new_hash)?,
            "V3" => sej_sec_cfg_hw_v3_encrypt(&new_hash, false)?,
            "V4" => sej_sec_cfg_hw_v3_encrypt(&new_hash, true)?,
            _ => return Err(format!("不支持的 hwtype: {}", self.hwtype)),
        };

        let mut result = seccfg_header.to_vec();
        result.extend_from_slice(&enc_hash);

        while !result.len().is_multiple_of(0x200) {
            result.push(0);
        }
        while result.len() < partition_size {
            result.push(0);
        }

        info!(
            "seccfg V4 修改成功: lock_state=0x{:08X} -> 0x{:08X}",
            self.lock_state, new_lock
        );
        Ok(result)
    }
}
