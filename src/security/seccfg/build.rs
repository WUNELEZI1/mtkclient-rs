//! SecCfg V4 头构建辅助（在线模式）
//!
//! - `build_v4_header` — 修改 lock_state + critical_lock_state，构造 28 字节头
//! - `build_v4_image_online` — 根据 hwtype 选择加密方式，拼装完整 image
//!   对齐 Python SecCfgV4.create()：SW→sej_sec_cfg_sw, V2→sej_sec_cfg_hw,
//!   V3→sej_sec_cfg_hw_V3(legacy=False), V4→sej_sec_cfg_hw_V3(legacy=True)

use log::info;
use sha2::{Digest, Sha256};

use crate::da::DAXFlash;
use crate::security::sej::{
    sej_sec_cfg_hw_encrypt, sej_sec_cfg_sw_encrypt,
    sej_hacc_sign, with_backend,
};

use super::v4::SecCfgV4;

pub(crate) fn build_v4_header(
    v4: &SecCfgV4,
    lockflag: &str,
) -> Result<([u8; 28], u32, u32, u32), String> {
    // critical_lock_state（LKCS）处理：
    //   unlock → LKCS_UNLOCK=1, lock → LKCS_LOCK=2
    //   偏移 0x10 的 critical_lock_state 必须正确声明，否则 Preloader/LK
    //   认为关键安全状态未声明 → seccfg 完整性校验失败 → red state
    //   lock_state（LKS）：unlock=LKS_UNLOCK(3), lock=LKS_DEFAULT=1
    let (new_lock, new_critical) = match lockflag {
        "unlock" => {
            if v4.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            (3u32, 1u32) // LKS_UNLOCK=3, LKCS_UNLOCK=1
        }
        "lock" => {
            if v4.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            (1u32, 2u32) // LKS_DEFAULT=1, LKCS_LOCK=2
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
        // 对齐 Python: endflag 固定 0x45454545
        0x45, 0x45, 0x45, 0x45,
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