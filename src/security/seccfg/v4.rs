//! SecCfg V4 结构（28 字节头 + 32 字节 SHA256/AES 哈希尾部）
//!
//! - 解析：识别 SW / V2 / V3 / V4 加密类型
//! - 构建：修改 lock_state + critical_lock_state，对齐 Python mtkclient

use log::info;
use sha2::{Digest, Sha256};

use crate::security::sej::{
    sej_sec_cfg_hw_decrypt, sej_sec_cfg_hw_encrypt, sej_sec_cfg_hw_v3_decrypt,
    sej_sec_cfg_hw_v3_encrypt, sej_sec_cfg_sw_decrypt, sej_sec_cfg_sw_encrypt,
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

        // 对齐 mtkclient: hash = data[seccfg_size - 0x20 : seccfg_size]
        // seccfg_size 通常为 0x3C，即 data[0x1C..0x3C]
        let seccfg_size = size as usize;
        let enc_hash = if seccfg_size >= 0x20 && data.len() >= seccfg_size {
            data[seccfg_size - 0x20..seccfg_size].to_vec()
        } else {
            return Err("seccfg_size 太小，无法读取 HACC 签名".to_string());
        };

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

        // 对齐 Python SecCfgV4.parse()：按 SW → V3 → V4(legacy) → V2 顺序尝试
        // 如果所有类型都不匹配（例如之前写入了错误签名的 seccfg），
        // 不报错退出，而是警告并默认 V4，允许用户重新写入修复
        let hwtype = if let Ok(dec) = sej_sec_cfg_sw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            "SW".to_string()
        } else if let Ok(dec) = sej_sec_cfg_hw_v3_decrypt(&enc_hash, false)
            && dec[..32] == expected_hash[..]
        {
            "V3".to_string()
        } else if let Ok(dec) = sej_sec_cfg_hw_v3_decrypt(&enc_hash, true)
            && dec[..32] == expected_hash[..]
        {
            "V4".to_string()
        } else if let Ok(dec) = sej_sec_cfg_hw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            "V2".to_string()
        } else {
            log::warn!(
                "[SECCFG] 签名哈希验证失败 (SW/V2/V3/V4 均不匹配)，可能是之前写入了错误签名。\
                 默认使用 V4 类型继续，写入时将重新生成正确签名。"
            );
            "V4".to_string()
        };

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
    /// critical_lock_state（LKCS）：unlock=LKCS_UNLOCK(1), lock=LKCS_LOCK(2)
    /// lock_state（LKS）：unlock=LKS_UNLOCK(3), lock=LKS_DEFAULT=1
    /// 偏移 0x10 的 critical_lock_state 必须正确声明，否则 Preloader/LK
    /// 认为关键安全状态未声明 → seccfg 完整性校验失败 → red state
    #[allow(dead_code)] // 预留：unlock-bootloader / lock-bootloader 命令调用入口
    pub(crate) fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let (new_lock, new_critical) = if lockflag == "unlock" {
            if self.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            (3u32, 1u32) // LKS_UNLOCK=3, LKCS_UNLOCK=1
        } else if lockflag == "lock" {
            if self.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            (1u32, 2u32) // LKS_DEFAULT=1, LKCS_LOCK=2
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
            new_critical.to_le_bytes()[0],
            new_critical.to_le_bytes()[1],
            new_critical.to_le_bytes()[2],
            new_critical.to_le_bytes()[3],
            self.sboot_runtime.to_le_bytes()[0],
            self.sboot_runtime.to_le_bytes()[1],
            self.sboot_runtime.to_le_bytes()[2],
            self.sboot_runtime.to_le_bytes()[3],
            // 对齐 Python: endflag 固定 0x45454545
            0x45, 0x45, 0x45, 0x45,
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
