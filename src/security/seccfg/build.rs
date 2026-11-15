//! SecCfg V4 头构建辅助（在线模式）
//!
//! - `build_v4_header` — 修改 lock_state + dm_verity_state，构造 28 字节头
//! - `build_v4_image_online` — 根据 hwtype 选择加密方式，拼装完整 image
//!   对齐 Python SecCfgV4.create()：SW→sej_sec_cfg_sw, V2→sej_sec_cfg_hw,
//!   V3→sej_sec_cfg_hw_V3(legacy=False), V4→sej_sec_cfg_hw_V3(legacy=True)

use crate::sha::Sha256;
use log::info;

use crate::da::DAXFlash;
use crate::security::sej::{
    sej_hacc_sign, sej_sec_cfg_hw_encrypt, sej_sec_cfg_sw_encrypt, with_backend,
};

use super::v4::SecCfgV4;

pub(crate) fn build_v4_header(
    v4: &SecCfgV4,
    lockflag: &str,
) -> Result<([u8; 28], u32, u32, u32), String> {
    // seccfg V4 头部字段（基于 libsec.a 逆向分析）:
    //   偏移 0x00 magic        = 0x4D4D4D4D
    //   偏移 0x04 seccfg_ver   = 0x4
    //   偏移 0x08 seccfg_size  = 0x3C
    //   偏移 0x0C lock_state   = LKS_UNLOCK(3) / LKS_DEFAULT(1)
    //   偏移 0x10 dm_verity_state = 0 表示正常（非0值如 0x01=DM_VERITY_GENERAL_ERROR 会触发 dm 校验错误）
    //   偏移 0x14 sboot_runtime = 0（对齐 mtkclient stage2.py）
    //   偏移 0x18 endflag      = 0x45454545
    //
    // 参考: https://m.book118.com/html/2022/1208/6022204135005024.shtm
    // seccfg_get_lock_state()     读取 g_seccfg_major[0] + 12 → lock_state
    // seccfg_get_dm_verity_state() 读取 g_seccfg_major[0] + 16 → dm_verity_state
    //
    // 旧版 mtkclient 错误地将偏移16视为 critical_lock_state，unlock时写入 0x01（DM_VERITY_GENERAL_ERROR）
    // 导致 dm-verity 校验错误。正确做法是 unlock/lock 时都保持 dm_verity_state = 0
    let (new_lock, new_dm_verity, new_sboot) = match lockflag {
        "unlock" => {
            if v4.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            (3u32, 0u32, 0u32) // LKS_UNLOCK=3, dm_verity=0, sboot=0
        }
        "lock" => {
            if v4.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            (1u32, 0u32, 0u32) // LKS_DEFAULT=1, dm_verity=0, sboot=0
        }
        _ => return Err("无效 lockflag".to_string()),
    };

    let endflag = v4.endflag.to_le_bytes();
    let header: [u8; 28] = [
        v4.magic.to_le_bytes()[0],
        v4.magic.to_le_bytes()[1],
        v4.magic.to_le_bytes()[2],
        v4.magic.to_le_bytes()[3],
        v4.seccfg_ver.to_le_bytes()[0],
        v4.seccfg_ver.to_le_bytes()[1],
        v4.seccfg_ver.to_le_bytes()[2],
        v4.seccfg_ver.to_le_bytes()[3],
        v4.seccfg_size.to_le_bytes()[0],
        v4.seccfg_size.to_le_bytes()[1],
        v4.seccfg_size.to_le_bytes()[2],
        v4.seccfg_size.to_le_bytes()[3],
        new_lock.to_le_bytes()[0],
        new_lock.to_le_bytes()[1],
        new_lock.to_le_bytes()[2],
        new_lock.to_le_bytes()[3],
        new_dm_verity.to_le_bytes()[0],
        new_dm_verity.to_le_bytes()[1],
        new_dm_verity.to_le_bytes()[2],
        new_dm_verity.to_le_bytes()[3],
        new_sboot.to_le_bytes()[0],
        new_sboot.to_le_bytes()[1],
        new_sboot.to_le_bytes()[2],
        new_sboot.to_le_bytes()[3],
        // 对齐 Python: endflag 固定 0x45454545（parse 已校验，此处回写解析到的原值）
        endflag[0],
        endflag[1],
        endflag[2],
        endflag[3],
    ];

    Ok((header, new_lock, new_dm_verity, new_sboot))
}

pub(crate) fn build_v4_image_online(
    da: &mut DAXFlash,
    v4: &SecCfgV4,
    lockflag: &str,
    partition_size: usize,
    hw_code: u16,
) -> Result<Vec<u8>, String> {
    let (header, new_lock, _, _) = build_v4_header(v4, lockflag)?;
    let digest = Sha256::digest(header);

    // 对齐 Python SecCfgV4.create()：根据 hwtype 选择加密方式
    // hwtype 在 parse() 时通过 SW→V3→V4(legacy)→V2 顺序解密验证确定
    let enc_hash = match v4.hwtype.as_str() {
        "SW" => {
            let enc = sej_sec_cfg_sw_encrypt(&digest)?;
            let mut out = [0u8; 32];
            out.copy_from_slice(&enc[..32.min(enc.len())]);
            out.to_vec()
        }
        "V2" => {
            // V2: 尝试 HACC，失败回退到 V2 软件加密
            match with_backend(da, || sej_hacc_sign(&digest, hw_code, false)) {
                Ok(sig) => sig,
                Err(_) => {
                    info!("[SECCFG] HACC 不可用，使用 V2 软件加密");
                    let enc = sej_sec_cfg_hw_encrypt(&digest)?;
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&enc[..32.min(enc.len())]);
                    out.to_vec()
                }
            }
        }
        "V3" => {
            // V3: HACC 是唯一正确路径（密钥来自芯片 HUID/HUK）
            let sig = with_backend(da, || sej_hacc_sign(&digest, hw_code, false))?;
            sig
        }
        "V4" => {
            // V4(legacy): HACC 是唯一正确路径
            let sig = with_backend(da, || sej_hacc_sign(&digest, hw_code, true))?;
            sig
        }
        _ => return Err(format!("不支持的 hwtype: {}", v4.hwtype)),
    };

    let mut result = header.to_vec();
    result.extend_from_slice(&enc_hash);
    while !result.len().is_multiple_of(0x200) {
        result.push(0);
    }
    while result.len() < partition_size {
        result.push(0);
    }

    info!(
        "seccfg V4 在线修改成功: lock_state=0x{:08X} -> 0x{:08X}",
        v4.lock_state, new_lock
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个仅用于头部改写测试的 V4 描述（不涉及加密/HACC）。
    fn v4(lock_state: u32, dm_verity_state: u32) -> SecCfgV4 {
        SecCfgV4 {
            magic: SecCfgV4::MAGIC,
            seccfg_ver: 4,
            seccfg_size: 0x3C,
            lock_state,
            dm_verity_state,
            sboot_runtime: 0,
            endflag: SecCfgV4::ENDFLAG,
            hwtype: "V4".to_string(),
        }
    }

    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn unlock_writes_lock_state_3_and_dm_verity_0() {
        // 输入里 dm_verity_state 故意置 1（DM_VERITY_GENERAL_ERROR），
        // unlock 必须把它清回 0，否则强解 BL 后开机会报 dm-verity error。
        let (header, lock, dm, sboot) = build_v4_header(&v4(1, 1), "unlock").unwrap();
        assert_eq!(lock, 3, "LKS_UNLOCK");
        assert_eq!(dm, 0, "dm_verity_state 必须为 0");
        assert_eq!(sboot, 0);
        assert_eq!(u32_at(&header, 0x0C), 3, "lock_state 在偏移 0x0C");
        assert_eq!(u32_at(&header, 0x10), 0, "dm_verity_state 在偏移 0x10");
        assert_eq!(u32_at(&header, 0x14), 0, "sboot_runtime 在偏移 0x14");
        assert_eq!(header.len(), 28);
    }

    #[test]
    fn lock_writes_lock_state_1_and_dm_verity_0() {
        let (header, lock, dm, _) = build_v4_header(&v4(3, 0x10), "lock").unwrap();
        assert_eq!(lock, 1, "LKS_DEFAULT");
        assert_eq!(dm, 0);
        assert_eq!(u32_at(&header, 0x0C), 1);
        assert_eq!(u32_at(&header, 0x10), 0);
    }

    #[test]
    fn already_unlocked_is_rejected() {
        assert!(build_v4_header(&v4(3, 0), "unlock").is_err());
        assert!(build_v4_header(&v4(1, 0), "lock").is_err());
        assert!(build_v4_header(&v4(1, 0), "bogus").is_err());
    }

    #[test]
    fn header_preserves_identity_fields() {
        let (header, ..) = build_v4_header(&v4(1, 0), "unlock").unwrap();
        assert_eq!(u32_at(&header, 0), SecCfgV4::MAGIC);
        assert_eq!(u32_at(&header, 4), 4);
        assert_eq!(u32_at(&header, 8), 0x3C);
        assert_eq!(u32_at(&header, 0x18), SecCfgV4::ENDFLAG);
    }
}
