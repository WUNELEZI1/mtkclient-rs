//! SecCfg V4 头构建辅助（在线模式）
//!
//! - `build_v4_header` — 仅修改 lock_state，构造 28 字节头
//! - `build_v4_image_online` — 用 DA SEJ 签名 + 拼装完整 image

use log::info;
use sha2::{Digest, Sha256};

use crate::da_extension::DAXFlash;
use crate::security::sej::{sej_hacc_sign, with_backend};

use super::v4::SecCfgV4;

pub(crate) fn build_v4_header(
    v4: &SecCfgV4,
    lockflag: &str,
) -> Result<([u8; 28], u32, u32, u32), String> {
    let (new_lock, new_critical) = match lockflag {
        "unlock" => {
            if v4.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            (3u32, v4.critical_lock_state)
        }
        "lock" => {
            if v4.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            (1u32, v4.critical_lock_state)
        }
        _ => return Err("无效 lockflag".to_string()),
    };

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
        new_critical.to_le_bytes()[0],
        new_critical.to_le_bytes()[1],
        new_critical.to_le_bytes()[2],
        new_critical.to_le_bytes()[3],
        v4.sboot_runtime.to_le_bytes()[0],
        v4.sboot_runtime.to_le_bytes()[1],
        v4.sboot_runtime.to_le_bytes()[2],
        v4.sboot_runtime.to_le_bytes()[3],
        v4.endflag.to_le_bytes()[0],
        v4.endflag.to_le_bytes()[1],
        v4.endflag.to_le_bytes()[2],
        v4.endflag.to_le_bytes()[3],
    ];

    Ok((header, new_lock, new_critical, v4.sboot_runtime))
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
    let enc_hash = with_backend(da, || sej_hacc_sign(digest.as_slice(), hw_code, None))?;

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
