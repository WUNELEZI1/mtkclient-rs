//! SecCfg V4 structure (28-byte header + 32-byte SHA256/AES hash tail)
//!
//! - Parse: detect the SW / V2 / V3 / V4 encryption type
//! - Build: modify lock_state + dm_verity_state, matching Python mtkclient

use crate::sha::Sha256;
use log::info;

use crate::security::sej::sej_sec_cfg_sw_decrypt;

/// SecCfg V4 parsing and modification
pub(crate) struct SecCfgV4 {
    pub(crate) magic: u32,
    pub(crate) seccfg_ver: u32,
    pub(crate) seccfg_size: u32,
    pub(crate) lock_state: u32,
    pub(crate) dm_verity_state: u32, // offset 0x10: dm-verity state (0 = normal)
    pub(crate) sboot_runtime: u32,
    pub(crate) endflag: u32,
    pub(crate) hwtype: String, // "SW", "V2", "V3", "V4"
}

impl SecCfgV4 {
    pub(crate) const MAGIC: u32 = 0x4D4D4D4D;
    pub(crate) const ENDFLAG: u32 = 0x45454545;

    /// Parse seccfg V4 data
    pub(crate) fn parse(data: &[u8]) -> Result<SecCfgV4, String> {
        if data.len() < 28 {
            return Err("seccfg 数据太小，无法解析 V4 头部".to_string());
        }

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let ver = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let size = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let lock = u32::from_le_bytes(data[12..16].try_into().unwrap());
        // Offset 0x10 is the dm-verity state (the historical name critical_lock_state was mtkclient's
        // misreading; a non-zero value triggers a dm-verity error, see the comment in build.rs)
        let dm_verity_state = u32::from_le_bytes(data[16..20].try_into().unwrap());
        let sboot = u32::from_le_bytes(data[20..24].try_into().unwrap());
        let endflag = u32::from_le_bytes(data[24..28].try_into().unwrap());

        if magic != Self::MAGIC || endflag != Self::ENDFLAG {
            return Err(format!(
                "非 V4 seccfg 结构 (magic=0x{:08X}, endflag=0x{:08X})",
                magic, endflag
            ));
        }

        // Mirrors mtkclient: hash = data[seccfg_size - 0x20 : seccfg_size]
        // seccfg_size is usually 0x3C, i.e. data[0x1C..0x3C]
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
            dm_verity_state.to_le_bytes()[0],
            dm_verity_state.to_le_bytes()[1],
            dm_verity_state.to_le_bytes()[2],
            dm_verity_state.to_le_bytes()[3],
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

        // Mirrors the C# port: treat the V4 format (magic=0x4D4D4D4D) as hwtype=V4
        // without software decryption verification (the HACC key comes from the chip HUID/HUK, which software cannot match)
        // The SW type is very rare; if present it can later be identified via HACC hardware decryption
        //
        // The original Python tried sej_sec_cfg_sw/v3/v2 decryption one by one to determine the type,
        // but only when custom_sej_hw (the HACC hardware backend) is available.
        // At parse time we have no HACC backend (DA Extensions are not initialized yet),
        // so decide by format alone and sign with HACC hardware later on write.
        let hwtype = if let Ok(dec) = sej_sec_cfg_sw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            "SW".to_string()
        } else {
            // Non-SW types all require HACC hardware verification, so default to the V4 format at parse time
            // sign_v4_digest uses HACC hardware signing (legacy=true) on write
            "V4".to_string()
        };

        let v4 = SecCfgV4 {
            magic,
            seccfg_ver: ver,
            seccfg_size: size,
            lock_state: lock,
            dm_verity_state,
            sboot_runtime: sboot,
            endflag,
            hwtype,
        };

        // Log the parse result (dm_verity_state/sboot_runtime help diagnose dm-verity and sboot runtime)
        info!(
            "seccfg V4 解析成功: hwtype={}, lock_state=0x{:08X}, dm_verity_state=0x{:08X}, sboot_runtime=0x{:08X}",
            v4.hwtype, v4.lock_state, v4.dm_verity_state, v4.sboot_runtime
        );

        Ok(v4)
    }
}
