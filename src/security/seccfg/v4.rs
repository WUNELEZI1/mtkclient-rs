//! SecCfg V4 结构（28 字节头 + 32 字节 SHA256/AES 哈希尾部）
//!
//! - 解析：识别 SW / V2 / V3 / V4 加密类型
//! - 构建：修改 lock_state + dm_verity_state，对齐 Python mtkclient

use log::info;
use sha2::{Digest, Sha256};

use crate::security::sej::{
    sej_sec_cfg_hw_encrypt, sej_sec_cfg_hw_v3_encrypt, sej_sec_cfg_sw_decrypt, sej_sec_cfg_sw_encrypt,
};

/// SecCfg V4 解析和修改
#[allow(dead_code)] // 预留：unlock/lock 功能使用
pub(crate) struct SecCfgV4 {
    pub(crate) magic: u32,
    pub(crate) seccfg_ver: u32,
    pub(crate) seccfg_size: u32,
    pub(crate) lock_state: u32,
    pub(crate) dm_verity_state: u32,  // 偏移 0x10: dm-verity 状态（0=正常）
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

        // 对齐 C# 移植版行为：V4 格式（magic=0x4D4D4D4D）直接判定 hwtype=V4
        // 不做软件解密验证（HACC 密钥来自芯片 HUID/HUK，软件模拟无法匹配）
        // SW 类型极少见，如果存在可在后续版本通过 HACC 硬件解密来识别
        //
        // Python 原版通过逐一尝试 sej_sec_cfg_sw/v3/v2 解密验证来确定类型，
        // 但前提是 custom_sej_hw（HACC 硬件后端）可用。
        // 我们在 parse 阶段没有 HACC 后端（此时 DA Extensions 尚未初始化），
        // 所以按格式直接判定即可，写入时再用 HACC 硬件签名。
        let hwtype = if let Ok(dec) = sej_sec_cfg_sw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            "SW".to_string()
        } else {
            // 非 SW 类型均需要 HACC 硬件验证，在 parse 阶段按 V4 格式默认
            // 写入时 build_v4_image_online 会用 HACC 硬件签名（legacy=true）
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
            dm_verity_state: crit_lock,
            sboot_runtime: sboot,
            endflag,
            hwtype,
        })
    }

    /// 修改 seccfg V4（lock/unlock）— 离线模式
    /// 偏移 0x10 实际是 dm_verity_state（0=正常），不是 critical_lock_state
    /// lock_state（LKS）：unlock=LKS_UNLOCK(3), lock=LKS_DEFAULT=1
    #[allow(dead_code)] // 预留：unlock-bootloader / lock-bootloader 命令调用入口
    pub(crate) fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let (new_lock, new_dm_verity) = if lockflag == "unlock" {
            if self.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            (3u32, 0u32) // LKS_UNLOCK=3, dm_verity=0
        } else if lockflag == "lock" {
            if self.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            (1u32, 0u32) // LKS_DEFAULT=1, dm_verity=0
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
            new_dm_verity.to_le_bytes()[0],
            new_dm_verity.to_le_bytes()[1],
            new_dm_verity.to_le_bytes()[2],
            new_dm_verity.to_le_bytes()[3],
            0u8, 0u8, 0u8, 0u8,  // sboot_runtime = 0
            // 对齐 Python: endflag 固定 0x45454545
            0x45,
            0x45,
            0x45,
            0x45,
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
