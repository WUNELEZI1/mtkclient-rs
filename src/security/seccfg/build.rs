//! SecCfg V4 header construction and HACC signing helpers
//!
//! - `build_v4_header` — modify lock_state + dm_verity_state, build a 28-byte header
//! - `sign_v4_digest` — sign the digest according to hwtype (SW/V2/V3/V4), shared by RealSession and unit tests

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
    // seccfg V4 header fields (based on reverse engineering of libsec.a):
    //   offset 0x00 magic        = 0x4D4D4D4D
    //   offset 0x04 seccfg_ver   = 0x4
    //   offset 0x08 seccfg_size  = 0x3C
    //   offset 0x0C lock_state   = LKS_UNLOCK(3) / LKS_DEFAULT(1)
    //   offset 0x10 dm_verity_state = 0 means normal (non-zero such as 0x01=DM_VERITY_GENERAL_ERROR triggers a dm verification error)
    //   offset 0x14 sboot_runtime = 0 (aligned with mtkclient stage2.py)
    //   offset 0x18 endflag      = 0x45454545
    //
    // Reference: https://m.book118.com/html/2022/1208/6022204135005024.shtm
    // seccfg_get_lock_state()      reads g_seccfg_major`0` + 12 → lock_state
    // seccfg_get_dm_verity_state() reads g_seccfg_major`0` + 16 → dm_verity_state
    //
    // Old mtkclient mistakenly treats offset 16 as critical_lock_state and writes 0x01 (DM_VERITY_GENERAL_ERROR) when unlocking,
    // causing dm-verity verification errors. The correct behavior is to keep dm_verity_state = 0 on both unlock and lock.
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
        // Align with Python: endflag is fixed at 0x45454545 (already validated by parse; write back the parsed original value here)
        endflag[0],
        endflag[1],
        endflag[2],
        endflag[3],
    ];

    Ok((header, new_lock, new_dm_verity, new_sboot))
}

/// Sign/encrypt the SHA256 digest of the 28-byte header according to hwtype.
///
/// Extracted so `session::RealSession` (capability-driven path) and the existing `unlock_bootloader`
/// share a single implementation, avoiding signing drift between the two paths.
pub(crate) fn sign_v4_digest(
    da: &mut DAXFlash,
    hwtype: &str,
    digest: &[u8; 32],
    hw_code: u16,
) -> Result<Vec<u8>, String> {
    // Align with Python SecCfgV4.create(): choose the encryption method by hwtype
    // hwtype is determined during parse() by trying decryption in SW→V3→V4(legacy)→V2 order
    let enc_hash = match hwtype {
        "SW" => {
            let enc = sej_sec_cfg_sw_encrypt(digest)?;
            let mut out = [0u8; 32];
            out.copy_from_slice(&enc[..32.min(enc.len())]);
            out.to_vec()
        }
        "V2" => {
            // V2: try HACC, fall back to V2 software encryption on failure
            match with_backend(da, || sej_hacc_sign(digest, hw_code, false)) {
                Ok(sig) => sig,
                Err(_) => {
                    info!("[SECCFG] HACC 不可用，使用 V2 软件加密");
                    let enc = sej_sec_cfg_hw_encrypt(digest)?;
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&enc[..32.min(enc.len())]);
                    out.to_vec()
                }
            }
        }
        "V3" => {
            // V3: HACC is the only correct path (keys come from the chip HUID/HUK)
            with_backend(da, || sej_hacc_sign(digest, hw_code, false))?
        }
        "V4" => {
            // V4(legacy): HACC is the only correct path
            with_backend(da, || sej_hacc_sign(digest, hw_code, true))?
        }
        other => return Err(format!("不支持的 hwtype: {}", other)),
    };
    Ok(enc_hash)
}

/// Pad the header + signature hash with zeros up to the partition size (for reuse by the capability-driven path).
pub(crate) fn pad_seccfg_image(
    header: &[u8; 28],
    enc_hash: &[u8],
    partition_size: usize,
) -> Vec<u8> {
    let mut result = header.to_vec();
    result.extend_from_slice(enc_hash);
    while !result.len().is_multiple_of(0x200) {
        result.push(0);
    }
    while result.len() < partition_size {
        result.push(0);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a V4 descriptor used only for header rewrite tests (no encryption/HACC involved).
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
        // dm_verity_state is deliberately set to 1 (DM_VERITY_GENERAL_ERROR) in the input;
        // unlock must clear it back to 0, otherwise booting after a forced BL unlock reports a dm-verity error.
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
