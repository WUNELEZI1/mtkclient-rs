use crate::preloader::Preloader;
use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::thread::sleep;
use std::time::Duration;

/// AES-256-CBC 类型别名（SW 模式 seccfg 签名用）
type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

// SEJ 软件模式密钥和 IV（对齐 Python hwcrypto_sej.py:sej_sec_cfg_sw）
const SEJ_SW_KEY: &[u8; 32] = b"25A1763A21BC854CD569DC23B4782B63";
const SEJ_IV: &[u8; 16] = &[
    0x57, 0x32, 0x5A, 0x5A, 0x12, 0x54, 0x97, 0x66, 0x12, 0x54, 0x97, 0x66, 0x57, 0x32, 0x5A, 0x5A,
];

// SEJ 硬件模式 g_CFG_RANDOM_PATTERN（对齐 Python hwcrypto_sej.py）
#[allow(dead_code)]
const G_CFG_RANDOM_PATTERN: [u32; 12] = [
    0x2D44BB70, 0xA744D227, 0xD0A9864B, 0x83FFC244, 0x7EC8266B, 0x43E80FB2, 0x01A6348A, 0x2067F9A0,
    0x54536405, 0xD546A6B1, 0x1CC3EC3A, 0xDE377A83,
];

// SEJ 硬件模式 g_HACC_CFG_1（AES-128-CBC 加密用 IV）
const G_HACC_CFG_1: [u32; 8] = [
    0x9ED40400, 0x00E884A1, 0xE3F083BD, 0x2F4E6D8A, 0xFF838E5C, 0xE940A0E3, 0x8D4DECC6, 0x45FC0989,
];

/// 自定义种子（对齐 Python CustomSeed 前 4 字节）
const CUSTOM_SEED_PREFIX: [u8; 4] = [0x00, 0xBE, 0x13, 0xBB];

/// SEJ 硬件模式 AES-128-CBC 加密密钥（固定 16 字节 0）
/// 对齐 Python HACC AES 硬件密钥流程：key 全零，通过 HUID/HUK 派生
const SEJ_HW_KEY: [u8; 16] = [0u8; 16];

/// DA extensions 模板（预编译的 da_x.bin）— 在 da_extension.rs 中使用
#[allow(dead_code)]
const DA_EXTENSIONS_TEMPLATE: &[u8] =
    include_bytes!("../mtkclient-2.0.1/mtkclient/payloads/da_x.bin");

// ==================== SEJ 加密函数 ====================
// 对齐 Python hwcrypto_sej.py:Sej

/// 软件模式 AES-256-CBC 解密（sej_sec_cfg_sw 解密）
/// 对齐 Python: sej_sec_cfg_sw(data, encrypt=False)
fn sej_sec_cfg_sw_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256CbcDec::new(SEJ_SW_KEY.into(), SEJ_IV.into());
    let mut buf = data.to_vec();
    // 数据长度必须是 16 的倍数
    if !buf.len().is_multiple_of(16) {
        return Err(format!(
            "sej_sec_cfg_sw 数据长度不是 16 的倍数: {}",
            buf.len()
        ));
    }
    cipher
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-256-CBC 解密失败: {:?}", e))?;
    Ok(buf)
}

/// 软件模式 AES-256-CBC 加密（sej_sec_cfg_sw 加密）
/// 对齐 Python: sej_sec_cfg_sw(data, encrypt=True)
fn sej_sec_cfg_sw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256CbcEnc::new(SEJ_SW_KEY.into(), SEJ_IV.into());
    let mut buf = data.to_vec();
    // 数据长度必须是 16 的倍数
    if !buf.len().is_multiple_of(16) {
        // 补齐到 16 的倍数（零填充）
        while !buf.len().is_multiple_of(16) {
            buf.push(0);
        }
    }
    let buf_len = buf.len();
    cipher
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-256-CBC 加密失败: {:?}", e))?;
    Ok(buf)
}

/// 生成 CustomSeed IV（用于 V3/V4 硬件模式 AES-128-CBC）
/// 对齐 Python hwcrypto_sej.py:sst_secure_algo_with_level 中 iv 计算
fn generate_custom_seed_iv() -> [u8; 16] {
    let seed = u32::from_le_bytes([
        CUSTOM_SEED_PREFIX[0],
        CUSTOM_SEED_PREFIX[1],
        CUSTOM_SEED_PREFIX[2],
        CUSTOM_SEED_PREFIX[3],
    ]);
    let rot = seed.rotate_left(16);
    let iv_parts: [u32; 4] = [
        seed,
        (!seed).wrapping_add(1), // ~seed & 0xFFFFFFFF
        rot,
        (!rot).wrapping_add(1),
    ];
    let mut iv = [0u8; 16];
    for (i, &part) in iv_parts.iter().enumerate() {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&part.to_le_bytes());
    }
    iv
}

/// 硬件模式 AES-128-CBC 加密（SEJ V3/V4 hwtype 签名用）
/// 对齐 Python: sej_sec_cfg_hw_V3 → hw_aes128_cbc_encrypt
/// 由于无法访问硬件 SEJ 寄存器，此处用软件模拟
/// 使用 g_HACC_CFG_1 作为 IV，AES-128-CBC
fn sej_sec_cfg_hw_v3_encrypt(data: &[u8], legacy: bool) -> Result<Vec<u8>, String> {
    // 构建 IV：legacy=True 使用 g_HACC_CFG_1，否则使用 CustomSeed IV
    let iv_bytes: [u8; 16] = if legacy {
        // V4 hwtype: 使用 g_HACC_CFG_1 前 4 个 dword 作为 IV
        let mut iv = [0u8; 16];
        for i in 0..4 {
            iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
        }
        iv
    } else {
        // V3 hwtype: 使用 CustomSeed 生成的 IV
        generate_custom_seed_iv()
    };

    // AES-128-CBC 加密
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv_bytes.into());
    let mut buf = data.to_vec();

    // 补齐到 16 的倍数（零填充，对齐 Python 无 padding 模式）
    while !buf.len().is_multiple_of(16) {
        buf.push(0);
    }
    let buf_len = buf.len();
    cipher
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密失败: {:?}", e))?;
    Ok(buf)
}

/// 硬件模式 V2 加密（sej_sec_cfg_hw）
/// 对齐 Python: sej_sec_cfg_hw
fn sej_sec_cfg_hw_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    // V2 使用 g_HACC_CFG_1 作为 IV
    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }

    let cipher = Aes128CbcEnc::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();

    while !buf.len().is_multiple_of(16) {
        buf.push(0);
    }
    let buf_len = buf.len();
    cipher
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, buf_len)
        .map_err(|e| format!("AES-128-CBC 加密 (V2) 失败: {:?}", e))?;
    Ok(buf)
}

// ==================== SecCfg V4 结构 ====================
// 对齐 Python seccfg.py:SecCfgV4
// V4 头部: magic(4) + seccfg_ver(4) + seccfg_size(4) + lock_state(4) +
//          critical_lock_state(4) + sboot_runtime(4) + endflag(4) = 28 字节
// 尾部: SHA256 hash(32 字节，经 AES 加密)

/// SecCfg V4 解析和修改
// 预留：unlock/lock 功能使用
#[allow(dead_code)]
struct SecCfgV4 {
    magic: u32,
    seccfg_ver: u32,
    seccfg_size: u32,
    lock_state: u32,
    critical_lock_state: u32,
    sboot_runtime: u32,
    endflag: u32,
    hwtype: String,     // "SW", "V2", "V3", "V4"
    full_data: Vec<u8>, // 完整 seccfg 数据（含 padding）
}

impl SecCfgV4 {
    const MAGIC: u32 = 0x4D4D4D4D;
    const ENDFLAG: u32 = 0x45454545;

    /// 解析 seccfg V4 数据
    fn parse(data: &[u8]) -> Result<SecCfgV4, String> {
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

        // 读取加密哈希（最后 32 字节）
        if data.len() < 0x20 {
            return Err("seccfg 数据太小，无法读取哈希".to_string());
        }
        let enc_hash = data[data.len() - 0x20..data.len()].to_vec();

        // 计算 SHA256（对头部 28 字节）
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

        // 尝试不同的 hwtype 验证哈希
        let mut hwtype = String::new();

        // 1. SW 模式：解密 enc_hash 看是否匹配 expected_hash
        if let Ok(dec) = sej_sec_cfg_sw_decrypt(&enc_hash)
            && dec[..32] == expected_hash[..]
        {
            hwtype = "SW".to_string();
        }

        // 2-4. V2/V3/V4 模式：由于需要 HUID/HUK 派生密钥，无法离线验证
        // 采用启发式方法：默认使用 V4 hwtype（常见于 MT6768/MT6771 等芯片）
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
            full_data: data.to_vec(),
        })
    }

    /// 修改 seccfg V4（lock/unlock）
    /// 对齐 C# 版正确行为：只修改 lock_state(0x0C)，不动 critical_lock_state/dm_verity(0x10)
    /// Python mtkclient 的 Bug：错误地将 offset 0x10 写为 01，导致 dm-verity corruption
    fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let new_lock = if lockflag == "unlock" {
            if self.lock_state == 3 {
                return Err("设备已解锁".to_string());
            }
            3u32 // LKS_UNLOCK
        } else if lockflag == "lock" {
            if self.lock_state == 1 {
                return Err("设备已上锁".to_string());
            }
            1u32 // LKS_DEFAULT
        } else {
            return Err("无效 lockflag".to_string());
        };

        // 只修改 lock_state，critical_lock_state(0x10) 和 sboot_runtime(0x14) 保持原值
        // 注意：C# 版不修改 offset 0x10，保持为 0x00
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
            // offset 0x10: critical_lock_state / dm_verity_state — 保持原值不变
            self.critical_lock_state.to_le_bytes()[0],
            self.critical_lock_state.to_le_bytes()[1],
            self.critical_lock_state.to_le_bytes()[2],
            self.critical_lock_state.to_le_bytes()[3],
            // offset 0x14: sboot_runtime — 保持原值不变
            self.sboot_runtime.to_le_bytes()[0],
            self.sboot_runtime.to_le_bytes()[1],
            self.sboot_runtime.to_le_bytes()[2],
            self.sboot_runtime.to_le_bytes()[3],
            self.endflag.to_le_bytes()[0],
            self.endflag.to_le_bytes()[1],
            self.endflag.to_le_bytes()[2],
            self.endflag.to_le_bytes()[3],
        ];

        // 计算新 SHA256（对 28 字节头部）
        let new_hash = Sha256::digest(seccfg_header);

        // 根据 hwtype 加密哈希
        let enc_hash = match self.hwtype.as_str() {
            "SW" => sej_sec_cfg_sw_encrypt(&new_hash)?,
            "V2" => sej_sec_cfg_hw_encrypt(&new_hash)?,
            "V3" => sej_sec_cfg_hw_v3_encrypt(&new_hash, false)?,
            "V4" => sej_sec_cfg_hw_v3_encrypt(&new_hash, true)?,
            _ => return Err(format!("不支持的 hwtype: {}", self.hwtype)),
        };

        // 组装: 头部 + 加密哈希
        let mut result = seccfg_header.to_vec();
        result.extend_from_slice(&enc_hash);

        // 补齐到分区大小（离线模式 8MB）或 0x200 对齐（在线模式）
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

// ==================== SecCfg V3 结构 ====================
// 对齐 Python seccfg.py:SecCfgV3
// V3 头部: info_header(16) + magic(4) + seccfg_ver(4) + seccfg_size(4) +
//          seccfg_enc_offset(4) + seccfg_enc_len(4) + sw_sec_lock_try(1) +
//          sw_sec_lock_done(1) + page_size(2) + page_count(4) = 44 字节
// 加密数据段 + endflag(4)

// 预留：unlock/lock 功能使用
#[allow(dead_code)]
struct SecCfgV3 {
    info_header: [u8; 16],
    magic: u32,
    seccfg_ver: u32,
    seccfg_size: u32,
    seccfg_enc_offset: u32,
    seccfg_enc_len: u32,
    sw_sec_lock_try: u8,
    sw_sec_lock_done: u8,
    page_size: u16,
    page_count: u32,
    seccfg_attr: u32,
    seccfg_status: u32,
    endflag: u32,
    hwtype: String,
    imginfo: Vec<[u8; 0x68]>, // 20 个 imginfo 条目
    seccfg_ext: Vec<u8>,      // 扩展数据 (0x1004 字节)
    full_data: Vec<u8>,
}

impl SecCfgV3 {
    const MAGIC: u32 = 0x4D4D4D4D;
    const ENDFLAG: u32 = 0x45454545;
    const ATTR_UNLOCK: u32 = 0x44444444;
    const ATTR_DEFAULT: u32 = 0x33333333;
    #[allow(dead_code)]
    const STATUS_COMPLETE: u32 = 0x43434343;
    #[allow(dead_code)]
    const STATUS_INCOMPLETE: u32 = 0x49494949;

    fn parse(data: &[u8]) -> Result<SecCfgV3, String> {
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

        // 加密数据段 = full_data[size - 0x2C - 4 .. size - 4]
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

        // 尝试解密（SW/V2/V3/V4）
        let mut hwtype = String::new();
        let mut decrypted = Vec::new();

        // 1. SW
        if let Ok(d) = sej_sec_cfg_sw_decrypt(enc_data) {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "SW".to_string();
                decrypted = d;
            }
        }

        // 2. V2
        if hwtype.is_empty()
            && let Ok(d) = sej_sec_cfg_hw_decrypt(enc_data)
        {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "V2".to_string();
                decrypted = d;
            }
        }

        // 3. V3
        if hwtype.is_empty()
            && let Ok(d) = sej_sec_cfg_hw_v3_decrypt(enc_data, false)
        {
            let first4 = &d[..std::cmp::min(4, d.len())];
            if first4 == b"IIII" || first4 == b"CCCC" || first4 == [0, 0, 0, 0] {
                hwtype = "V3".to_string();
                decrypted = d;
            }
        }

        // 4. V4 (legacy=True)
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

        // 解析解密后的数据
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
            full_data: data.to_vec(),
        })
    }

    fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        let new_attr = if lockflag == "unlock" {
            if self.seccfg_attr != Self::ATTR_DEFAULT
                && self.seccfg_attr != 0x6003 // ATTR_MP_DEFAULT
                && self.seccfg_attr != 0x6002 // ATTR_CUSTOM
                && self.seccfg_attr != 0x6001 // ATTR_VERIFIED
                && self.seccfg_attr != 0x6000
            // ATTR_LOCK
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

        // 构建内部数据
        let mut inner = Vec::new();
        for img in &self.imginfo {
            inner.extend_from_slice(img);
        }
        inner.extend_from_slice(&0u32.to_le_bytes()); // siu_status (placeholder)
        inner.extend_from_slice(&self.seccfg_status.to_le_bytes());
        inner.extend_from_slice(&new_attr.to_le_bytes());
        inner.extend_from_slice(&self.seccfg_ext);

        // 根据 hwtype 加密
        let enc_data = match self.hwtype.as_str() {
            "SW" => sej_sec_cfg_sw_encrypt(&inner)?,
            "V2" => sej_sec_cfg_hw_encrypt(&inner)?,
            "V3" => sej_sec_cfg_hw_v3_encrypt(&inner, false)?,
            "V4" => sej_sec_cfg_hw_v3_encrypt(&inner, true)?,
            _ => return Err(format!("不支持的 hwtype: {}", self.hwtype)),
        };

        // 组装完整数据
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

        // 补齐到 0x200 对齐，再补齐到分区大小
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

/// 硬件模式 AES-128-CBC 解密（V3/V4 用）
fn sej_sec_cfg_hw_v3_decrypt(data: &[u8], legacy: bool) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    let iv_bytes: [u8; 16] = if legacy {
        let mut iv = [0u8; 16];
        for i in 0..4 {
            iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
        }
        iv
    } else {
        generate_custom_seed_iv()
    };

    let cipher = Aes128CbcDec::new(&SEJ_HW_KEY.into(), &iv_bytes.into());
    let mut buf = data.to_vec();

    if !buf.len().is_multiple_of(16) {
        return Err("V3 解密数据长度不是 16 的倍数".to_string());
    }

    cipher
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-128-CBC 解密失败: {:?}", e))?;
    Ok(buf)
}

/// 硬件模式 V2 解密
fn sej_sec_cfg_hw_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::Aes128;
    type Aes128CbcDec = cbc::Decryptor<Aes128>;

    let mut iv = [0u8; 16];
    for i in 0..4 {
        iv[i * 4..(i + 1) * 4].copy_from_slice(&G_HACC_CFG_1[i].to_le_bytes());
    }

    let cipher = Aes128CbcDec::new(&SEJ_HW_KEY.into(), &iv.into());
    let mut buf = data.to_vec();

    if !buf.len().is_multiple_of(16) {
        return Err("V2 解密数据长度不是 16 的倍数".to_string());
    }

    cipher
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| format!("AES-128-CBC 解密 (V2) 失败: {:?}", e))?;
    Ok(buf)
}

// ==================== 离线模式 seccfg 处理 ====================
// 对齐 C# 版行为：不连接设备，直接读取/修改 seccfg 文件

/// 自动检测 seccfg 版本
pub fn detect_seccfg_version(data: &[u8]) -> Result<&str, String> {
    if data.len() >= 4 && data[0..4] == [0x4D, 0x4D, 0x4D, 0x4D] {
        Ok("V4")
    } else if data.len() >= 16 && &data[0..16] == b"AND_SECCFG_v\x00\x00\x00\x00" {
        Ok("V3")
    } else {
        Err("未知 seccfg 版本".into())
    }
}

/// 离线模式解锁 seccfg
pub fn seccfg_unlock_offline(input_file: &str) -> Result<(), String> {
    let data = std::fs::read(input_file).map_err(|e| format!("无法读取文件: {}", e))?;

    let version = detect_seccfg_version(&data)?;
    println!("检测到 {} 锁", version);

    let new_data = match version {
        "V4" => {
            println!("  HACC init");
            println!("  HACC run");
            let v4 = SecCfgV4::parse(&data)?;
            println!("  HACC terminate");
            println!("  HwType: {}", v4.hwtype);
            println!("  关闭DM验证 ...");
            v4.create("unlock", 0x800000)?
        }
        "V3" => {
            println!("  HACC init");
            println!("  HACC run");
            let v3 = SecCfgV3::parse(&data)?;
            println!("  HACC terminate");
            println!("  HwType: {}", v3.hwtype);
            v3.create("unlock", 0x800000)?
        }
        _ => return Err(format!("不支持的版本: {}", version)),
    };

    // 生成输出文件名
    let output_path = if let Some(idx) = input_file.rfind('.') {
        format!("{}_unlock{}", &input_file[..idx], &input_file[idx..])
    } else {
        format!("{}_unlock", input_file)
    };

    std::fs::write(&output_path, &new_data).map_err(|e| format!("写入文件失败: {}", e))?;

    println!("  镜像格式为RAW ...");
    // 模拟 C# 进度条输出
    println!(
        "  进度: |████████████████████████████████████████| 100.0% 写入 ({} B => {} B)",
        new_data.len(),
        new_data.len()
    );
    println!("  成功写入SecCfg");
    println!("成功生成 unlock 文件: {}", output_path);
    Ok(())
}

/// 离线模式锁定 seccfg
pub fn seccfg_lock_offline(input_file: &str) -> Result<(), String> {
    let data = std::fs::read(input_file).map_err(|e| format!("无法读取文件: {}", e))?;

    let version = detect_seccfg_version(&data)?;
    println!("检测到 {} 锁", version);

    let new_data = match version {
        "V4" => {
            let v4 = SecCfgV4::parse(&data)?;
            println!("  HwType: {}", v4.hwtype);
            v4.create("lock", 0x800000)?
        }
        "V3" => {
            let v3 = SecCfgV3::parse(&data)?;
            println!("  HwType: {}", v3.hwtype);
            v3.create("lock", 0x800000)?
        }
        _ => return Err(format!("不支持的版本: {}", version)),
    };

    let output_path = if let Some(idx) = input_file.rfind('.') {
        format!("{}_lock{}", &input_file[..idx], &input_file[idx..])
    } else {
        format!("{}_lock", input_file)
    };

    std::fs::write(&output_path, &new_data).map_err(|e| format!("写入文件失败: {}", e))?;

    println!(
        "  进度: |████████████████████████████████████████| 100.0% 写入 ({} B => {} B)",
        new_data.len(),
        new_data.len()
    );
    println!("  成功写入SecCfg");
    println!("成功生成 lock 文件: {}", output_path);
    Ok(())
}

// DA 文件 region 结构
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DaRegion {
    buf_offset: u32, // 在文件中的偏移
    len: u32,        // 大小
    start_addr: u32, // 加载地址
    sig_len: u32,    // 签名长度
}

/// 从 DA header 数据中解析 region 信息
fn parse_da_regions(header: &[u8], region_start: usize, entry_region_count: u16) -> Vec<DaRegion> {
    let mut regions = Vec::new();

    for j in 0..entry_region_count as usize {
        let region_offset = region_start + j * 20;
        if region_offset + 20 > header.len() {
            break;
        }

        let buf_offset = u32::from_le_bytes([
            header[region_offset],
            header[region_offset + 1],
            header[region_offset + 2],
            header[region_offset + 3],
        ]);
        let len = u32::from_le_bytes([
            header[region_offset + 4],
            header[region_offset + 5],
            header[region_offset + 6],
            header[region_offset + 7],
        ]);
        let start_addr = u32::from_le_bytes([
            header[region_offset + 8],
            header[region_offset + 9],
            header[region_offset + 10],
            header[region_offset + 11],
        ]);
        let sig_len = u32::from_le_bytes([
            header[region_offset + 16],
            header[region_offset + 17],
            header[region_offset + 18],
            header[region_offset + 19],
        ]);

        regions.push(DaRegion {
            buf_offset,
            len,
            start_addr,
            sig_len,
        });
    }

    regions
}

// 解析 MTK AllInOne DA 文件
fn parse_da_header(
    da_data: &[u8],
    target_hw_code: u16,
) -> Result<(u32, Vec<DaRegion>, bool), String> {
    if da_data.len() < 0x6C {
        return Err("DA 文件太小，无法解析头".to_string());
    }

    // 检查是否是 MTK AllInOne DA 格式
    let header = &da_data[0..0x68];
    let is_allinone = header.contains(&0x4D) && header.contains(&0x54) && header.contains(&0x4B);

    if is_allinone {
        // 读取 DA 数量
        let count_da =
            u32::from_le_bytes([da_data[0x68], da_data[0x69], da_data[0x6A], da_data[0x6B]]);

        // 检查是否是 v6 格式
        let is_v6 = header.windows(8).any(|window| window == b"MTK_DA_v6");

        // 检查是否是旧版加载器
        let old_ldr: bool;
        let offset: usize;

        if da_data.len() > 0x6C + 0xD8 + 1 {
            let marker = [da_data[0x6C + 0xD8], da_data[0x6C + 0xD8 + 1]];
            if marker == [0xDA, 0xDA] {
                offset = 0xD8;
                old_ldr = true;
            } else {
                offset = 0xDC;
                old_ldr = false;
            }
        } else {
            offset = 0xDC;
            old_ldr = false;
        }

        // 查找适合目标 HW Code 的 DA
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];

            // 读取 DA 信息
            let magic = u16::from_le_bytes([da_header[0], da_header[1]]);
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);
            let _hw_sub_code = u16::from_le_bytes([da_header[4], da_header[5]]);
            let _hw_version = u16::from_le_bytes([da_header[6], da_header[7]]);

            // 新版加载器有 sw_version + reserved1 (4 字节)
            let _sw_version: u16;
            let _page_size: u16;
            let _entry_region_index: u16;
            let entry_region_count: u16;
            let region_start: usize;

            if old_ldr {
                _sw_version = 0;
                _page_size = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved = u16::from_le_bytes([da_header[10], da_header[11]]);
                _entry_region_index = u16::from_le_bytes([da_header[12], da_header[13]]);
                entry_region_count = u16::from_le_bytes([da_header[14], da_header[15]]);
                region_start = 16;
            } else {
                _sw_version = u16::from_le_bytes([da_header[8], da_header[9]]);
                let _reserved1 = u16::from_le_bytes([da_header[10], da_header[11]]);
                _page_size = u16::from_le_bytes([da_header[12], da_header[13]]);
                let _reserved3 = u16::from_le_bytes([da_header[14], da_header[15]]);
                _entry_region_index = u16::from_le_bytes([da_header[16], da_header[17]]);
                entry_region_count = u16::from_le_bytes([da_header[18], da_header[19]]);
                region_start = 20;
            }

            // 跳过不匹配的 DA，静默处理
            if hw_code != target_hw_code {
                continue;
            }

            info!("找到匹配的 DA {}，HW Code: 0x{:04X}", i, hw_code);

            let regions = parse_da_regions(da_header, region_start, entry_region_count);

            return Ok((magic as u32, regions, is_v6));
        }

        // 如果没有找到匹配的 HW Code，返回第一个有效的 DA（hw_code 不为 0）
        warn!(
            "未找到 HW Code 0x{:04X} 的 DA，尝试使用第一个有效的 DA",
            target_hw_code
        );
        for i in 0..count_da {
            let da_offset = 0x6C + (i as usize) * offset;
            if da_offset + offset > da_data.len() {
                continue;
            }

            let da_header = &da_data[da_offset..da_offset + offset];
            let hw_code = u16::from_le_bytes([da_header[2], da_header[3]]);

            if hw_code != 0 {
                let region_start = da_offset + 16;
                let entry_region_count = if offset >= 16 {
                    u16::from_le_bytes([da_header[14], da_header[15]])
                } else {
                    0
                };

                let mut regions = parse_da_regions(da_header, region_start, entry_region_count);

                // 如果没有读取到 regions，使用默认的 Stage1 和 Stage2 region
                if regions.is_empty() {
                    warn!("备选 DA 没有 region 信息，使用默认值");
                    // 默认 Stage1 region
                    regions.push(DaRegion {
                        buf_offset: 0x376C,
                        len: 0x270,
                        start_addr: 0x200000,
                        sig_len: 0x0,
                    });
                    // 默认 Stage2 region
                    regions.push(DaRegion {
                        buf_offset: 0x39E4,
                        len: 0xE660,
                        start_addr: 0x80000000,
                        sig_len: 0x100,
                    });
                }

                info!("使用 DA {} (HW Code: 0x{:04X}) 作为备选", i, hw_code);
                return Ok((0, regions, is_v6));
            }
        }

        Err("在 AllInOne DA 中未找到有效的 DA 配置".to_string())
    } else {
        // 传统 DA 格式解析
        let magic = u32::from_le_bytes([da_data[0], da_data[1], da_data[2], da_data[3]]);
        let hw_code = u16::from_le_bytes([da_data[4], da_data[5]]);
        let entry_region_count = u16::from_le_bytes([da_data[16], da_data[17]]);

        if hw_code != target_hw_code {
            warn!(
                "DA HW Code (0x{:04X}) 与目标 (0x{:04X}) 不匹配",
                hw_code, target_hw_code
            );
        }

        let is_v6 = magic == 0x5644415F;

        let regions = parse_da_regions(da_data, 0x6C, entry_region_count);

        Ok((magic, regions, is_v6))
    }
}

// XFlash 命令常量
pub const CMD_MAGIC: u32 = 0xFEEEEEEF;
const CMD_SYNC_SIGNAL: u32 = 0x434E5953;
const CMD_SETUP_ENVIRONMENT: u32 = 0x010100;
const CMD_SETUP_HW_INIT_PARAMS: u32 = 0x010101;
#[allow(dead_code)]
const CMD_INIT_EXT_RAM: u32 = 0x01000A;
const CMD_BOOT_TO: u32 = 0x010008;
const CMD_READ_DATA: u32 = 0x010005; // XFlash 读分区命令
pub const CMD_WRITE_DATA: u32 = 0x010004; // 写入数据命令

pub const CMD_FORMAT: u32 = 0x010003; // 格式化命令
#[allow(dead_code)]
const CMD_SEND_DA: u8 = 0xD7;
#[allow(dead_code)]
const CMD_JUMP_DA: u8 = 0xD5;
pub const SET_META_BOOT_MODE: u32 = 0x020006;

// 存储类型（保留供未来使用）
#[allow(dead_code)]
const STORAGE_EMMC: u32 = 1; // EMMC 存储
#[allow(dead_code)]
const STORAGE_UFS: u32 = 2; // UFS 存储

// 分区类型 (EMMC)（保留供未来使用）
#[allow(dead_code)]
const PARTTYPE_USER: u32 = 8; // USER 分区
#[allow(dead_code)]
const PARTTYPE_BOOT1: u32 = 1; // BOOT1 分区
#[allow(dead_code)]
const PARTTYPE_BOOT2: u32 = 2; // BOOT2 分区

/// pack3: 生成 XFlash 参数包头 (magic(4) + data_type(4) + length(4))
pub fn pack3(magic: u32, data_type: u32, length: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&magic.to_le_bytes());
    buf[4..8].copy_from_slice(&data_type.to_le_bytes());
    buf[8..12].copy_from_slice(&length.to_le_bytes());
    buf
}

/// ACK 响应枚举 — 替代裸 u32 返回值
#[derive(Debug, PartialEq)]
pub(crate) enum AckResult {
    /// status == 0，设备就绪，继续传输
    Continue,
    /// status != 0，传输终止或设备报错
    Terminated(u32),
}

/// EMMC 信息结构
pub struct EmmcInfo {
    pub boot1_size: u64,
    pub boot2_size: u64,
}

/// DAXFlash 结构体，处理 XFlash 协议
pub struct DAXFlash<'a> {
    pub preloader: &'a mut Preloader,
    emi: Option<Vec<u8>>,
    emi_version: u32,
    pub(crate) da2_data: Vec<u8>,
    pub(crate) da2_base_addr: u64,
    pub daext: bool,
    pub(crate) last_gpt_data: Option<Vec<u8>>,
    storage_setup_done: bool,
}

impl<'a> DAXFlash<'a> {
    pub fn new(preloader: &'a mut Preloader) -> Self {
        DAXFlash {
            preloader,
            emi: None,
            emi_version: 0,
            da2_data: Vec::new(),
            da2_base_addr: 0x40000000,
            daext: false,
            last_gpt_data: None,
            storage_setup_done: false,
        }
    }

    /// 读取 2 字节数据
    fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0; 2];
        self.preloader.device.read(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// 读取 4 字节数据
    #[allow(dead_code)]
    fn rdword(&mut self) -> Result<u32, String> {
        let mut buf = [0; 4];
        self.preloader.device.read(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// 从 preloader 文件中提取 EMI 数据
    pub fn load_preloader_emi(&mut self, preloader_path: &str) -> Result<bool, String> {
        info!("加载 preloader 文件: {}", preloader_path);

        // 读取 preloader 文件
        let preloader_data = match std::fs::read(preloader_path) {
            Ok(data) => data,
            Err(e) => {
                warn!("无法打开 preloader 文件: {}, 继续执行", e);
                return Ok(false);
            }
        };

        info!("preloader 文件大小: {} 字节", preloader_data.len());

        // 提取 EMI 数据
        match self.extract_emi(&preloader_data) {
            Ok((version, emi_data)) => {
                let emi_len = emi_data.len();
                self.emi = Some(emi_data);
                self.emi_version = version;
                info!(
                    "成功提取 EMI 数据，版本: {}, 大小: {} 字节",
                    version, emi_len
                );
                Ok(true)
            }
            Err(e) => {
                warn!("提取 EMI 数据失败: {}, 继续执行", e);
                Ok(false)
            }
        }
    }

    /// 提取 EMI 数据的内部方法
    fn extract_emi(&self, data: &[u8]) -> Result<(u32, Vec<u8>), String> {
        // 查找标记
        let marker = b"\x4D\x4D\x4D\x01\x38\x00\x00\x00";
        let idx = data
            .windows(marker.len())
            .position(|window| window == marker);

        let mut emi_data = data.to_vec();

        if let Some(idx) = idx {
            info!("找到 EMI 标记，偏移: 0x{:08X}", idx);
            emi_data = data[idx..].to_vec();

            // 读取 mlen 和 siglen
            if emi_data.len() >= 0x30 {
                let mlen = u32::from_le_bytes(emi_data[0x20..0x24].try_into().unwrap());
                let siglen = u32::from_le_bytes(emi_data[0x2C..0x30].try_into().unwrap());
                info!("mlen: 0x{:08X}, siglen: 0x{:08X}", mlen, siglen);

                // 截取数据
                if mlen as usize <= emi_data.len() {
                    emi_data = emi_data[..mlen as usize - siglen as usize].to_vec();
                }

                // 读取 dramsize
                if emi_data.len() >= 4 {
                    let dramsize =
                        u32::from_le_bytes(emi_data[emi_data.len() - 4..].try_into().unwrap());
                    info!("dramsize: 0x{:08X}", dramsize);

                    if dramsize == 0 && emi_data.len() >= 0x804 {
                        emi_data = emi_data[..emi_data.len() - 0x800].to_vec();
                        if emi_data.len() >= 4 {
                            let dramsize = u32::from_le_bytes(
                                emi_data[emi_data.len() - 4..].try_into().unwrap(),
                            );
                            info!("调整后 dramsize: 0x{:08X}", dramsize);
                        }
                    }

                    // 截取 EMI 数据
                    if dramsize > 0 && emi_data.len() >= (dramsize + 4) as usize {
                        emi_data = emi_data
                            [emi_data.len() - (dramsize + 4) as usize..emi_data.len() - 4]
                            .to_vec();
                    }
                }
            }
        }

        // 查找 MTK_BLOADER_INFO_v 字符串
        let bldrstring = b"MTK_BLOADER_INFO_v";
        let idx = emi_data
            .windows(bldrstring.len())
            .position(|window| window == bldrstring);

        if let Some(idx) = idx {
            info!("找到 MTK_BLOADER_INFO_v，偏移: 0x{:08X}", idx);

            // 提取版本号
            let version_str = &emi_data[idx + bldrstring.len()..idx + bldrstring.len() + 2];
            let version = String::from_utf8_lossy(version_str)
                .trim_end_matches('\0')
                .parse::<u32>()
                .unwrap_or_default();
            info!("EMI 版本: {}", version);

            // ⚠️ 关键修复：如果 MTK_BLOADER_INFO_v 在偏移 0，返回整个 emi_data
            // 对齐 Python m_extract_emi: if idx == 0 and damode == XFLASH: return ver, data
            if idx == 0 {
                debug!("MTK_BLOADER_INFO_v 在偏移 0，使用完整 EMI 数据（含 header）");
                debug!("EMI 数据大小: {} 字节", emi_data.len());
                return Ok((version, emi_data));
            }

            // 原有逻辑：只提取 MTK_BIN 后的数据
            let mtk_bin_idx = emi_data.windows(7).position(|window| window == b"MTK_BIN");
            if let Some(mtk_bin_idx) = mtk_bin_idx {
                let emi = emi_data[mtk_bin_idx + 0xC..].to_vec();
                info!("EMI 数据大小: {} 字节", emi.len());
                return Ok((version, emi));
            }
        }

        Err("未找到 EMI 数据".to_string())
    }

    /// 从 dump 的 preloader 数据中提取 EMI 并设置到结构体中
    #[allow(dead_code)]
    pub fn extract_emi_from_data(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let (version, emi) = self.extract_emi(data)?;
        self.emi_version = version;
        self.emi = Some(emi.clone());
        Ok(emi)
    }

    /// 读取 XFlash 协议数据
    /// 流程：读取 12 字节头（magic + type + length）→ 验证 magic → 读取数据
    /// 返回：读取到的数据长度（如果是 4 字节则返回 u32 值）
    #[allow(dead_code)]
    fn xread(&mut self) -> Result<u32, String> {
        // 读取 12 字节的 XFlash 头
        let mut header = [0; 12];
        self.preloader.device.read(&mut header)?;

        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let _data_type = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap());

        if magic != CMD_MAGIC {
            return Err(format!("XFlash 头 magic 错误: 0x{:08X}", magic));
        }

        // 读取数据
        if length > 0 {
            let mut data = vec![0; length as usize];
            self.preloader.device.read(&mut data)?;

            // 如果数据是 4 字节，返回 u32 值
            if length == 4 {
                return Ok(u32::from_le_bytes(data[0..4].try_into().unwrap()));
            }
        }

        Ok(0)
    }

    /// XFlash 同步命令
    /// Python: sync() → 只 xsend(CMD_SYNC_SIGNAL)，不读 status 也不读 response
    fn xflash_sync(&mut self) -> Result<bool, String> {
        debug!("执行 XFlash 同步命令...");

        // Python: self.sync() → self.xsend(self.Cmd.SYNC_SIGNAL)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SYNC_SIGNAL.to_le_bytes())?;

        Ok(true)
    }

    /// 设置环境
    /// Python: xsend(CMD_SETUP_ENVIRONMENT) → send_param(20字节) → status()
    #[allow(dead_code)]
    pub fn setup_env(&mut self) -> Result<bool, String> {
        debug!("设置环境...");

        // xsend(CMD_SETUP_ENVIRONMENT)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SETUP_ENVIRONMENT.to_le_bytes())?;

        // send_param(20字节): da_log_level, log_channel, system_os, ufs_provision, 0x0
        let param: [u8; 20] = [
            0x00, 0x00, 0x00, 0x00, // da_log_level = 0
            0x01, 0x00, 0x00, 0x00, // log_channel = 1
            0x01, 0x00, 0x00, 0x00, // system_os = OS_LINUX = 1
            0x00, 0x00, 0x00, 0x00, // ufs_provision = 0
            0x00, 0x00, 0x00, 0x00, // 0x0
        ];
        let param_pkt = pack3(CMD_MAGIC, 0x01, 20);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("setup_env status error: 0x{:08X}", st));
        }

        debug!("环境设置成功");
        Ok(true)
    }

    /// 初始化硬件
    /// Python: xsend(CMD_SETUP_HW_INIT_PARAMS) → send_param(pack("<I", 0x0)) → status()
    #[allow(dead_code)]
    pub fn setup_hw_init(&mut self) -> Result<bool, String> {
        info!("初始化硬件...");

        // xsend(CMD_SETUP_HW_INIT_PARAMS)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_SETUP_HW_INIT_PARAMS.to_le_bytes())?;

        // send_param(pack("<I", 0x0)): 4字节参数 = 0x0
        let param = 0x0u32.to_le_bytes();
        let param_pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        // status
        let st = self.status()?;
        if st != 0 {
            return Err(format!("setup_hw_init status error: 0x{:08X}", st));
        }

        debug!("硬件初始化成功");
        Ok(true)
    }

    /// 上传数据到设备
    /// 流程：分块发送数据 → 每 0x2000 字节发空包 → 发送结束空包 → 读取校验和状态
    #[allow(dead_code)]
    pub fn upload_data(&mut self, data: &[u8], gen_chksum: u16) -> Result<bool, String> {
        debug!("开始上传 DA 数据，总大小: {} 字节", data.len());

        let maxinsize = 64; // 与 Python 版一致
        let mut bytestowrite = data.len();
        let mut pos = 0;

        while bytestowrite > 0 {
            let sz = std::cmp::min(bytestowrite, maxinsize);
            let chunk = &data[pos..pos + sz];

            debug!("发送数据块，偏移: 0x{:X}, 大小: {} 字节", pos, sz);
            self.preloader.device.write(chunk)?;

            bytestowrite -= sz;
            pos += sz;

            // 每 0x2000 字节发送一个空包
            if pos % 0x2000 == 0 {
                debug!("发送空包...");
                self.preloader.device.write(&[])?;
                // 增加短暂延迟，避免发送过快
                sleep(Duration::from_millis(10));
            }
        }

        // 发送结束空包
        debug!("发送结束空包...");
        self.preloader.device.write(&[])?;
        sleep(Duration::from_millis(120));

        // 读取校验和和状态
        debug!("读取 DA 上传结果...");
        let checksum = self.rword()?;
        let status = self.rword()?;

        debug!("校验和: 0x{:04X}, 状态: 0x{:04X}", checksum, status);

        if gen_chksum != checksum && checksum != 0 {
            warn!("上传校验和不匹配！");
        }

        if status > 0xFF {
            return Err(format!("DA 发送状态错误: 0x{:04X}", status));
        }

        Ok(true)
    }

    /// 发送 EMI 数据初始化 DRAM
    /// 流程：发送 INIT_EXT_RAM → 发送 EMI 数据 → 验证状态
    #[allow(dead_code)]
    pub fn send_emi(&mut self, emi: &[u8]) -> Result<bool, String> {
        debug!("发送 EMI 数据初始化 DRAM...");
        debug!(
            "[EMI DEBUG] len={}, first 64 bytes={}",
            emi.len(),
            emi[..std::cmp::min(64, emi.len())]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        // 1. xsend(INIT_EXT_RAM)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader
            .device
            .write(&CMD_INIT_EXT_RAM.to_le_bytes())?;

        // 2. status() - Python reads immediately, no sleep
        let st = self.status()?;
        debug!("[EMI DEBUG] INIT_EXT_RAM status=0x{:08X}", st);
        if st != 0 {
            return Err(format!("INIT_EXT_RAM status error: 0x{:08X}", st));
        }

        // 3. sleep(0.01) - Python sleeps AFTER status check
        sleep(Duration::from_millis(10));

        // 4. xsend(len(emi)) - Python sends header + length value
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader
            .device
            .write(&(emi.len() as u32).to_le_bytes())?;

        // 5. send_param([emi]) - Python sends param header + data in 512-byte chunks
        let param_pkt = pack3(CMD_MAGIC, 0x01, emi.len() as u32);
        self.preloader.device.write(&param_pkt)?;

        // Python send_param splits data into 0x200 (512) byte chunks
        let chunk_size = 0x200;
        let mut pos = 0;
        let mut remaining = emi.len();
        while remaining > 0 {
            let dsize = std::cmp::min(remaining, chunk_size);
            self.preloader.device.write(&emi[pos..pos + dsize])?;
            pos += dsize;
            remaining -= dsize;
        }

        // 6. status() - wait for EMI config complete (Python takes ~1.1s)
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        let st3 = self.status()?;
        self.preloader.device.set_timeout(orig_timeout);

        if st3 != 0 {
            return Err(format!("EMI data status error: 0x{:08X}", st3));
        }

        debug!("EMI 数据发送成功");
        Ok(true)
    }

    /// Boot 到指定地址
    /// 对齐 Python boot_to + send_data 流程
    pub(crate) fn boot_to(
        &mut self,
        addr: u32,
        da: &[u8],
        display: bool,
        timeout: f32,
    ) -> Result<bool, String> {
        if display {
            debug!("Boot 到地址: 0x{:08X}, 大小: {} 字节", addr, da.len());
        }

        // Python: self.xsend(self.Cmd.BOOT_TO)
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_BOOT_TO.to_le_bytes())?;

        // Python: self.status()
        let st = self.status()?;
        debug!("[BOOT_TO DEBUG] status1=0x{:08X} (after xsend BOOT_TO)", st);
        if st != 0 {
            return Err(format!("boot_to status1 error: 0x{:08X}", st));
        }

        // Python: param = pack("<QQ", addr, len(da)) + usbwrite(pkt1) + usbwrite(param)
        let param_len: u64 = da.len() as u64;
        let mut param = Vec::with_capacity(16);
        param.extend_from_slice(&(addr as u64).to_le_bytes());
        param.extend_from_slice(&param_len.to_le_bytes());
        let pkt1 = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&pkt1)?;
        self.preloader.device.write(&param)?;

        // Python: self.send_data(da) — 发送 12 字节头 + 分块 64 字节数据
        let pkt2 = pack3(CMD_MAGIC, 0x01, da.len() as u32);
        self.preloader.device.write(&pkt2)?;

        let maxinsize = 64;
        let mut remaining = da.len();
        let mut pos = 0;
        let mut send_failed = false;

        while remaining > 0 {
            let chunk_size = std::cmp::min(remaining, maxinsize);
            let chunk = &da[pos..pos + chunk_size];

            match self.preloader.device.write(chunk) {
                Ok(_) => {
                    pos += chunk_size;
                    remaining -= chunk_size;
                }
                Err(e) => {
                    if display {
                        warn!(
                            "Stage2 数据传输中断（设备开始执行，剩余 {} 字节）: {}",
                            remaining, e
                        );
                    }
                    send_failed = true;
                    break;
                }
            }

            if pos % 0x2000 == 0 && !send_failed {
                self.preloader.device.write(&[]).ok();
                sleep(Duration::from_millis(10));
            }
        }

        if !send_failed {
            // Python send_data: 数据发送完成后直接读 status，不发送 ZLP
            let _ = self.status();
        }

        // Python: time.sleep(timeout) — 等待设备执行
        let sleep_ms = (timeout * 1000.0) as u64;
        sleep(Duration::from_millis(sleep_ms));
        debug!("[BOOT_TO DEBUG] slept {}ms, reading status2...", sleep_ms);

        // Python: try: status = self.status() except: ...
        // 设备可能已经重新枚举，status 读取失败是正常的
        match self.status() {
            Ok(st2) => {
                // Python: 接受 0x434E5953 (SYNC_SIGNAL="CYNS") 或 0x0 作为成功
                if st2 == 0x434E5953 || st2 == 0x0 {
                    if display {
                        debug!("Boot 成功");
                    }
                    Ok(true)
                } else {
                    // Python: 其他状态码只打印 error，不抛异常
                    if display {
                        warn!("boot_to 状态: 0x{:08X}", st2);
                    }
                    Ok(true) // 不返回错误，继续执行
                }
            }
            Err(_) => {
                // Python: status 读取失败时打印 error 但继续
                if display {
                    warn!("boot_to status 读取失败（设备已重新枚举）");
                }
                Ok(true)
            }
        }
    }

    /// 上传第一阶段 DA
    /// 对照 Python xflash_lib.py:upload_da1
    pub fn upload_da1(&mut self) -> Result<bool, String> {
        debug!("上传 XFlash 阶段 1...");

        let mut file =
            File::open("MTK_DA_V5.bin").map_err(|e| format!("无法打开 DA 文件: {}", e))?;
        let mut da_data = Vec::new();
        file.read_to_end(&mut da_data)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;

        let (_magic, regions, _is_v6) = parse_da_header(&da_data, 0x6768)?;

        if regions.len() < 2 {
            return Err("DA 文件格式错误，无法找到 Stage1 region".to_string());
        }

        let stage1 = &regions[1];
        let da1_buf_offset = stage1.buf_offset;
        let da1_len = stage1.len;
        let da1_address = stage1.start_addr;
        let _da1_sig_len = stage1.sig_len;

        debug!(
            "  偏移: 0x{:08X}, 大小: 0x{:08X}, 地址: 0x{:08X}",
            da1_buf_offset, da1_len, da1_address
        );

        let da1_start = da1_buf_offset as usize;
        let da1_end = da1_start + da1_len as usize;
        if da1_end > da_data.len() {
            return Err("DA 文件格式错误，Stage1 数据超出文件范围".to_string());
        }

        let mut da1_patched = da_data[da1_start..da1_end].to_vec();
        debug!("应用 DA1 patch...");
        Self::patch_da1(&mut da1_patched);

        if !self
            .preloader
            .send_da(da1_address, da1_len, 0, &da1_patched)?
        {
            return Err("发送 DA 失败".to_string());
        }

        debug!("成功上传 stage 1，跳转中...");

        self.preloader.jump_da(da1_address)?;

        // Give device time to start DA execution
        std::thread::sleep(Duration::from_millis(100));

        // Python: sync = self.usbread(1) 等待 0xC0
        let orig_timeout = self.preloader.device.get_timeout();
        self.preloader
            .device
            .set_timeout(Duration::from_millis(5000));

        let mut sync = [0u8; 1];
        self.preloader.device.read(&mut sync)?;
        self.preloader.device.set_timeout(orig_timeout);
        if sync[0] != 0xC0 {
            return Err(format!("Error DA 同步: 0x{:02X}", sync[0]));
        }
        debug!("DA 同步 OK (0xC0)");

        // Python: self.sync() 发送 XFlash SYNC_SIGNAL
        self.xflash_sync()?;

        // Python: self.setup_env()
        self.setup_env()?;

        // Python: self.setup_hw_init()
        self.setup_hw_init()?;

        // Python: res = self.xread(); if res == pack("<I", self.Cmd.SYNC_SIGNAL)
        let resp = self.xread()?;
        if resp != CMD_SYNC_SIGNAL {
            return Err(format!("Error jumping to DA: got 0x{:08X}", resp));
        }
        debug!("已接收 DA 同步信号");

        Ok(true)
    }

    /// 上传第二阶段 DA
    /// 流程：检查是否需要 EMI → 发送 EMI → 调用 boot_to 上传 Stage2 → reinit
    pub fn upload_da2(&mut self) -> Result<bool, String> {
        debug!("上传 XFlash 阶段 2...");

        let mut file =
            File::open("MTK_DA_V5.bin").map_err(|e| format!("无法打开 DA 文件: {}", e))?;
        let mut da_data = Vec::new();
        file.read_to_end(&mut da_data)
            .map_err(|e| format!("读取 DA 文件失败: {}", e))?;

        let (_magic, regions, _is_v6) = parse_da_header(&da_data, 0x6768)?;

        if regions.len() < 3 {
            return Err("DA 文件格式错误，无法找到 Stage2 region".to_string());
        }

        let stage2 = &regions[2];
        let da2_buf_offset = stage2.buf_offset;
        let da2_len = stage2.len;
        let da2_address = stage2.start_addr;
        let da2_sig_len = stage2.sig_len;

        debug!(
            "  偏移: 0x{:08X}, 大小: 0x{:08X}, 地址: 0x{:08X}",
            da2_buf_offset, da2_len, da2_address
        );

        let da2_start = da2_buf_offset as usize;
        let da2_end_raw = da2_start + da2_len as usize;
        if da2_end_raw > da_data.len() {
            return Err("DA 文件格式错误，Stage2 数据超出文件范围".to_string());
        }

        let sig_len = da2_sig_len as usize;
        let da2_size_before = da2_end_raw - da2_start;
        let mut da2_data = if sig_len > 0 && da2_size_before > sig_len {
            da_data[da2_start..da2_end_raw - sig_len].to_vec()
        } else {
            da_data[da2_start..da2_end_raw].to_vec()
        };
        if sig_len > 0 {
            debug!(
                "  DA2 原始大小: 0x{:X} ({}) 字节",
                da2_size_before, da2_size_before
            );
            debug!(
                "  DA2 截断后大小: 0x{:X} ({}) 字节 (已截断 0x{:X} 字节签名)",
                da2_data.len(),
                da2_data.len(),
                sig_len
            );
        }

        debug!("应用 DA2 patch...");
        Self::patch_da2(&mut da2_data);

        self.da2_data = da2_data.clone();
        self.da2_base_addr = da2_address as u64;

        if !self.boot_to(da2_address, &da2_data, true, 0.5)? {
            return Err("上传 Stage2 失败".to_string());
        }

        debug!("Stage2 上传成功");
        Ok(true)
    }

    /// 发送 devctrl 命令
    /// Python: 任何阶段失败都返回 b""，不抛异常
    pub(crate) fn send_devctrl(
        &mut self,
        cmd: u32,
        param: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        // xsend(Cmd.DEVICE_CTRL) — DEVICE_CTRL = 0x010009
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&0x010009u32.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            if st != 0xC0010004 {
                warn!("send_devctrl DEVICE_CTRL 阶段1 状态: 0x{:08X}", st);
            }
            return Ok(vec![]);
        }

        // xsend(cmd)
        let pkt2 = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt2)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;

        let st2 = self.status()?;
        if st2 != 0 {
            if st2 != 0xC0010004 {
                warn!("send_devctrl(0x{:06X}) 状态: 0x{:08X}", cmd, st2);
            }
            return Ok(vec![]);
        }

        if let Some(p) = param {
            let pkt3 = pack3(CMD_MAGIC, 0x01, p.len() as u32);
            self.preloader.device.write(&pkt3)?;
            self.preloader.device.write(p)?;
            let st3 = self.status()?;
            if st3 != 0 {
                warn!("send_devctrl param 状态: 0x{:08X}", st3);
                return Ok(vec![]);
            }
        } else {
            let resp = self.xread_data()?;
            debug!(
                "[send_devctrl] cmd=0x{:06X} xread returned {} bytes",
                cmd,
                resp.len()
            );
            return Ok(resp);
        }

        Ok(vec![])
    }

    /// 读取 status (4 字节小端)
    pub(crate) fn status(&mut self) -> Result<u32, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("status magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut tmp = vec![0u8; length as usize];
            self.preloader.device.read(&mut tmp)?;
            if length == 4 {
                let val = u32::from_le_bytes(tmp[..4].try_into().unwrap());
                // Python special case: if status == 0xFEEEEEEF, return 0
                if val == 0xFEEEEEEF {
                    return Ok(0);
                }
                return Ok(val);
            } else if length == 2 {
                return Ok(u16::from_le_bytes(tmp[..2].try_into().unwrap()) as u32);
            }
        }
        Ok(0)
    }

    /// 读取 XFlash 数据并返回 Vec
    pub(crate) fn xread_data(&mut self) -> Result<Vec<u8>, String> {
        let mut hdr = [0u8; 12];
        self.preloader.device.read(&mut hdr)?;
        let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let length = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        if magic != CMD_MAGIC {
            return Err(format!("xread magic error: 0x{:08X}", magic));
        }
        if length > 0 {
            let mut data = vec![0u8; length as usize];
            self.preloader.device.read(&mut data)?;
            Ok(data)
        } else {
            Ok(vec![])
        }
    }

    /// xsend(cmd) + status
    #[allow(dead_code)]
    fn xsend_cmd(&mut self, cmd: u32) -> Result<(), String> {
        let magic = CMD_MAGIC.to_le_bytes();
        self.preloader.device.write(&magic)?;
        let data_type = 0x01u32.to_le_bytes();
        self.preloader.device.write(&data_type)?;
        let length = 4u32.to_le_bytes();
        self.preloader.device.write(&length)?;
        self.preloader.device.write(&cmd.to_le_bytes())?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("xsend_cmd status error: 0x{:08X}", st));
        }
        Ok(())
    }

    /// send_param: 发送参数包
    #[allow(dead_code)]
    fn send_param(&mut self, data: &[u8]) -> Result<(), String> {
        let pkt = pack3(CMD_MAGIC, 0x01, data.len() as u32);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(data)?;
        let st = self.status()?;
        if st != 0 {
            return Err(format!("send_param status error: 0x{:08X}", st));
        }
        Ok(())
    }

    /// 获取连接代理（brom 或 preloader）
    /// Python: 返回 b"" 或 None 时视为失败
    fn get_connection_agent(&mut self) -> Result<String, String> {
        let data = self.send_devctrl(0x010102, None)?; // GET_CONNECTION_AGENT
        if data.is_empty() {
            return Err("get_connection_agent returned empty".to_string());
        }
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// 设置重置键
    fn set_reset_key(&mut self, key: u32) -> Result<(), String> {
        self.send_devctrl(0x010103, Some(&key.to_le_bytes()))?;
        Ok(())
    }

    /// 设置校验级别
    fn set_checksum_level(&mut self, level: u32) -> Result<(), String> {
        self.send_devctrl(0x010104, Some(&level.to_le_bytes()))?;
        Ok(())
    }

    /// 获取过期日期
    fn get_expire_date(&mut self) -> Result<Vec<u8>, String> {
        let data = self.send_devctrl(0x010105, None)?;
        if data.is_empty() {
            return Err("get_expire_date returned empty".to_string());
        }
        Ok(data)
    }

    /// 获取 SLA 状态
    fn get_sla_status(&mut self) -> Result<u32, String> {
        let data = self.send_devctrl(0x01010E, None)?; // SLA_ENABLED_STATUS
        if data.len() >= 4 {
            Ok(u32::from_le_bytes(data[..4].try_into().unwrap()))
        } else {
            Err("sla_status empty".to_string())
        }
    }

    /// 重新初始化（获取 EMMC/芯片信息等）
    fn reinit(&mut self) -> Result<(), String> {
        // GET_RAM_INFO
        match self.send_devctrl(0x010107, None) {
            Ok(data) if data.len() >= 24 => {
                let sram_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let dram_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
                info!(
                    "  SRAM 类型: 0x{:08X}, DRAM 大小: 0x{:08X}",
                    sram_type, dram_size
                );
            }
            _ => {}
        }

        // GET_CHIP_ID
        match self.send_devctrl(0x010106, None) {
            Ok(data) if data.len() >= 10 => {
                let hw_code = u16::from_le_bytes([data[0], data[1]]);
                info!("  芯片 HW Code: 0x{:04X}", hw_code);
            }
            _ => {}
        }

        // GET_DA_VERSION
        if let Ok(data) = self.send_devctrl(0x01010A, None) {
            let ver = String::from_utf8_lossy(&data);
            info!("  DA 版本: {}", ver);
        }

        // GET_RANDOM_ID
        if let Ok(data) = self.send_devctrl(0x01010B, None) {
            debug!("  Random ID: {:02X?}", data);
        }

        // GET_EMMC_INFO
        match self.send_devctrl(0x01010C, None) {
            Ok(data) if data.len() >= 80 => {
                let emmc_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let emmc_block_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                let user_size = u64::from_le_bytes([
                    data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
                ]);
                info!(
                    "  EMMC 类型: {}, block_size: 0x{:08X}, user_size: 0x{:016X}",
                    emmc_type, emmc_block_size, user_size
                );
            }
            _ => {}
        }

        Ok(())
    }

    /// 上传 DA（完整流程，对齐 Python upload_da）
    pub fn upload_da(&mut self) -> Result<bool, String> {
        debug!("开始 DA 加载流程...");

        if !self.upload_da1()? {
            return Err("Stage1 上传失败".to_string());
        }

        match self.get_expire_date() {
            Ok(d) if !d.is_empty() => debug!("  过期日期: {:02X?}", d),
            Err(e) => warn!("get_expire_date 失败 (可能不支持): {}", e),
            _ => {}
        }

        if let Err(e) = self.set_reset_key(0x68) {
            warn!("set_reset_key 失败 (可能不支持): {}", e);
        }

        if let Err(e) = self.set_checksum_level(0x0) {
            warn!("set_checksum_level 失败 (可能不支持): {}", e);
        }

        let conn_agent = match self.get_connection_agent() {
            Ok(agent) => agent,
            Err(e) => {
                warn!("get_connection_agent 失败: {}", e);
                "brom".to_string()
            }
        };
        debug!("  连接代理: {}", conn_agent);

        if conn_agent == "brom" {
            if let Some(emi_data) = self.emi.clone() {
                debug!("发送 EMI 数据...");
                self.send_emi(&emi_data)?;
            } else {
                warn!("未找到 EMI 数据，跳过发送");
            }
        }

        if !self.upload_da2()? {
            return Err("Stage2 上传失败".to_string());
        }

        match self.get_sla_status() {
            Ok(sla) => {
                if sla != 0 {
                    debug!("  DA SLA 已启用: 0x{:08X}", sla);
                } else {
                    debug!("  DA SLA 未启用");
                }
            }
            Err(e) => warn!("get_sla_status 失败: {}", e),
        }

        if let Err(e) = self.reinit() {
            warn!("reinit 失败: {}", e);
        }

        // 10. 加载 DA extensions（对齐 Python xflash_lib.py 第 1247-1258 行）
        //      Python: daextdata = self.xft.patch()
        //              if self.boot_to(addr=0x4FFF0000, da=daextdata):
        //                  ret = self.send_devctrl(XCmd.CUSTOM_ACK)
        //                  status = self.status()
        //                  if status == 0x0 and unpack("<I", ret)[0] == 0xA1A2A3A4:
        debug!("正在加载 DA extensions...");

        // 清空 USB 输入缓冲区：reinit() 后设备可能发送残留数据，
        // 如果不干净，会污染后续 BOOT_TO 命令的响应。
        {
            let mut drain_buf = [0u8; 64];
            loop {
                let orig_timeout = self.preloader.device.get_timeout();
                self.preloader.device.set_timeout(Duration::from_millis(50));
                match self.preloader.device.read(&mut drain_buf) {
                    Ok(0) | Err(_) => {
                        self.preloader.device.set_timeout(orig_timeout);
                        break;
                    }
                    Ok(n) => {
                        debug!("[DRAIN] discarded {} bytes", n);
                    }
                }
                self.preloader.device.set_timeout(orig_timeout);
            }
        }

        if let Some(ext_data) = self.generate_da_extensions() {
            match self.boot_to(0x4FFF0000, &ext_data, true, 0.5) {
                Ok(_) => {
                    // Python 第 1251 行：boot_to 成功后立刻发送 CUSTOM_ACK
                    // 给 extensions 一点初始化时间
                    sleep(Duration::from_millis(100));
                    if let Ok(ack) = self.send_devctrl(0x0F0000, None) {
                        // Python 第 1252 行：send_devctrl 后还要读一次 status
                        let status = self.status();
                        let status_ok = match status {
                            Ok(s) => s == 0,
                            Err(_) => false,
                        };
                        if ack.len() >= 4 {
                            let magic = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
                            if status_ok && magic == 0xA1A2A3A4 {
                                // Python 第 1256 行：CUSTOM_ACK 成功后立即调用 custom_set_storage
                                // CUSTOM_SET_STORAGE = 0x0F0005，参数：0=eMMC, 1=UFS
                                if self
                                    .send_devctrl(0x0F0005, Some(&0u32.to_le_bytes()))
                                    .is_ok()
                                {
                                    info!("DA Extensions 加载成功，存储类型已设置为 eMMC");
                                    self.daext = true;
                                } else {
                                    warn!("custom_set_storage 失败，extensions 功能可能受限");
                                    self.daext = true;
                                }
                            } else {
                                warn!(
                                    "DA extensions CUSTOM_ACK 验证失败 (status={:?}, magic=0x{:08X})",
                                    status, magic
                                );
                            }
                        } else {
                            warn!("DA extensions CUSTOM_ACK 响应为空 (len={})", ack.len());
                        }
                    }
                }
                Err(e) => {
                    warn!("boot_to(extensions) 失败: {}", e);
                }
            }
        }
        if !self.daext {
            warn!("DA extensions 未启用");
        }

        info!("DA 加载完成");
        Ok(true)
    }

    /// 获取 EMMC 信息（Boot1/Boot2 大小）
    /// 对齐 Python: send_devctrl(0x01010C, None) → get_emmc_info
    pub fn get_emmc_info(&mut self) -> Result<EmmcInfo, String> {
        let data = self.send_devctrl(0x01010C, None)?;
        if data.len() < 8 {
            return Err("EMMC info 数据太短".to_string());
        }
        let boot1_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as u64;
        let boot2_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
        Ok(EmmcInfo {
            boot1_size,
            boot2_size,
        })
    }

    #[allow(dead_code)]
    /// 初始化存储协议（只执行一次）
    /// 对齐 Python custom_set_storage(): cmd(CUSTOM_SET_STORAGE) → xsend(storage_type) → status()
    fn setup_storage(&mut self) -> Result<(), String> {
        if self.storage_setup_done {
            return Ok(());
        }
        debug!("[setup_storage] 初始化存储协议...");

        // 1. DEVICE_CTRL
        self.send_devctrl(0x010009, None)?;

        // 2. CUSTOM_SET_STORAGE (0x0F0005) + param (0 = EMMC)
        self.send_devctrl(0x0F0005, Some(&0u32.to_le_bytes()))?;

        self.storage_setup_done = true;
        debug!("[setup_storage] 完成");
        Ok(())
    }

    /// 读取 flash 数据，返回原始字节
    /// 对齐 Python xflash_lib.py:879-891 (filename="" 分支):
    ///   get_packet_length → cmd_read_data → xread 循环 (header+data) → ack
    /// 注意：filename="" 分支没有 readflash_final 包，设备不会发送 final
    pub(crate) fn readflash_data(&mut self, addr: u64, size: u64) -> Result<Vec<u8>, String> {
        // 1. get_packet_length (send_devctrl 0x040007 + status)
        // Python: get_packet_length() → send_devctrl → if resp != "": status()
        let _ = self.send_devctrl(0x040007, None);
        let _ = self.status();

        // 2. cmd_read_data: xsend(CMD_READ_DATA) → status → send_param → status
        let pkt = pack3(CMD_MAGIC, 0x01, 4);
        self.preloader.device.write(&pkt)?;
        self.preloader.device.write(&CMD_READ_DATA.to_le_bytes())?;

        let st = self.status()?;
        if st != 0 {
            return Err(format!("READ_DATA status=0x{:08X}", st));
        }

        // send_param: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32)
        let mut param = Vec::with_capacity(56);
        param.extend_from_slice(&1u32.to_le_bytes()); // storage = 1 (eMMC)
        param.extend_from_slice(&8u32.to_le_bytes()); // parttype = 8 (USER)
        param.extend_from_slice(&addr.to_le_bytes());
        param.extend_from_slice(&size.to_le_bytes());
        param.extend_from_slice(&[0u8; 32]); // NandExtension 全零
        let param_pkt = pack3(CMD_MAGIC, 0x01, param.len() as u32);
        self.preloader.device.write(&param_pkt)?;
        self.preloader.device.write(&param)?;

        let st2 = self.status()?;
        if st2 != 0 {
            return Err(format!("send_param status=0x{:08X}", st2));
        }

        // 3. 数据读取循环 — 对齐 Python xflash_lib.py:879-891 (filename="" 分支)
        let mut buffer = Vec::new();
        let mut remaining = size as usize;

        while remaining > 0 {
            // 读 12 字节头
            let mut hdr = [0u8; 12];
            match self.preloader.device.read_exact(&mut hdr) {
                Ok(0) => {
                    // ZLP 或空响应 — 设备无更多数据，正常结束
                    debug!("[readflash_data] ZLP on header read, ending loop");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("[readflash_data] read header error (end of transfer): {}", e);
                    break;
                }
            }

            let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

            if magic != CMD_MAGIC {
                // 读到非预期数据，可能是残留状态包，容错退出
                debug!(
                    "[readflash_data] bad magic: 0x{:08X} at offset {}, ending loop",
                    magic,
                    buffer.len()
                );
                break;
            }

            // 读数据
            let mut data = vec![0u8; slength as usize];
            if slength > 0 {
                if let Err(e) = self.preloader.device.read_exact(&mut data) {
                    debug!("[readflash_data] read data error: {}", e);
                    break;
                }
            }

            // 追加数据
            buffer.extend_from_slice(&data);
            remaining = remaining.saturating_sub(data.len());

            // 发 ACK 并读 status
            match self.ack() {
                AckResult::Continue => {
                    debug!("[readflash_data] ack: Continue");
                }
                AckResult::Terminated(code) => {
                    debug!("[readflash_data] ack: Terminated(0x{:X}), ending loop", code);
                    break;
                }
            }
        }

        debug!("[readflash_data] total read {} bytes", buffer.len());
        Ok(buffer)
    }

    /// 发送 ACK 并读取设备响应
    /// 写入 12B header(CMD_MAGIC + 0x01 + 4) + 4B 零值 → 读取 status
    /// 返回 AckResult::Continue（可继续）或 AckResult::Terminated（终止）
    pub(crate) fn ack(&mut self) -> AckResult {
        let hdr = pack3(CMD_MAGIC, 0x01, 4);
        if self.preloader.device.write(&hdr).is_err() {
            return AckResult::Terminated(1);
        }
        if self.preloader.device.write(&0u32.to_le_bytes()).is_err() {
            return AckResult::Terminated(2);
        }
        match self.status() {
            Ok(0) => AckResult::Continue,
            Ok(n) => AckResult::Terminated(n),
            Err(_) => AckResult::Terminated(3),
        }
    }

    pub fn patch_vbmeta(&mut self, mode: u32) -> Result<(), String> {
        info!("修补 vbmeta，模式: {}", mode);
        Ok(())
    }

    pub fn close_device(&mut self, reset: bool) {
        if reset {
            if let Err(e) = self.preloader.jump_bl() {
                warn!("jump_bl 失败: {}", e);
            } else {
                info!("已发送 JUMP_BL 命令，设备将重启");
            }
        }
    }

    /// 获取 EMI 数据（调试模式使用）
    pub fn get_emi_data(&self) -> Option<&Vec<u8>> {
        self.emi.as_ref()
    }

    /// 获取 DA extensions 数据（调试模式使用）
    pub fn get_extensions_data(&self) -> Option<Vec<u8>> {
        if self.da2_data.is_empty() {
            None
        } else {
            self.generate_da_extensions()
        }
    }

    /// 获取最后一次 GPT 读取的原始数据（调试模式使用）
    pub fn get_last_gpt_data(&self) -> Result<&Vec<u8>, String> {
        self.last_gpt_data
            .as_ref()
            .ok_or_else(|| "无 GPT 数据".to_string())
    }
}

/// 解析 GPT 分区表（独立函数，不依赖 USB）
/// 自动检测偏移：如果 0x200 处没有 EFI PART，则尝试偏移 4 字节（跳过可能的状态包）
pub fn parse_gpt_from_data(data: &[u8]) -> Result<(), String> {
    println!("  数据大小: {} 字节", data.len());

    // 搜索 EFI PART 签名
    let base = match data.windows(8).position(|w| w == b"EFI PART") {
        Some(off) => off,
        None => return Err("未找到 GPT 签名 (EFI PART)".to_string()),
    };

    println!("GPT 头部 (偏移=0x{:X}):", base);

    // 验证 revision
    let revision = u32::from_le_bytes(data[base + 8..base + 12].try_into().unwrap());
    if revision != 0x10000 {
        return Err(format!("GPT revision 不匹配: 0x{:08X}", revision));
    }

    // 读取 header 字段
    let num_part_entries = u32::from_le_bytes(data[base + 80..base + 84].try_into().unwrap());
    let part_entry_size = u32::from_le_bytes(data[base + 84..base + 88].try_into().unwrap());
    let part_entry_start_lba = u64::from_le_bytes(data[base + 72..base + 80].try_into().unwrap());
    let first_usable_lba = u64::from_le_bytes(data[base + 32..base + 40].try_into().unwrap());

    println!("  修订版本: 0x{:08X}", revision);
    println!(
        "  头部大小: {} 字节",
        u32::from_le_bytes(data[base + 12..base + 16].try_into().unwrap())
    );
    println!(
        "  当前 LBA: {}",
        u64::from_le_bytes(data[base + 24..base + 32].try_into().unwrap())
    );
    println!("  首个可用 LBA: {}", first_usable_lba);
    println!("  分区项 LBA: {}", part_entry_start_lba);
    println!("  分区数量: {}", num_part_entries);
    println!("  分区项大小: {} 字节", part_entry_size);

    // 分区表从绝对偏移 part_entry_start_lba * 512 开始
    let mut table_start = (part_entry_start_lba as usize) * 512;

    // 如果分区表位置前 4 字节全零（状态包残渣），跳过
    if table_start + 4 <= data.len() && data[table_start..table_start + 4].iter().all(|&b| b == 0) {
        table_start += 4;
    }

    println!("\n分区信息:");
    println!("{:<30} {:<16} {:<16}", "分区名称", "起始地址", "大小");

    let mut count = 0;
    for i in 0..num_part_entries {
        let entry_offset = table_start + (i as usize) * (part_entry_size as usize);

        // 边界检查
        if entry_offset + part_entry_size as usize > data.len() {
            println!("  ... 缓冲区不足，仅显示 {} 个分区", count);
            break;
        }

        let entry = &data[entry_offset..entry_offset + part_entry_size as usize];

        // 检查条目是否全零（结束标记）
        let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
        if unique_guid_zero {
            break;
        }

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let start = first_lba.saturating_mul(512);
        let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

        // UTF-16LE 名称在偏移 56..112
        let name_utf16: Vec<u16> = (0..28)
            .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
            .collect();
        let name = String::from_utf16_lossy(&name_utf16)
            .trim_end_matches('\0')
            .to_string();

        count += 1;
        println!("{:<30} 0x{:014X} 0x{:014X}", name, start, size);
    }

    println!("\n共 {} 个分区", count);
    Ok(())
}

/// 从 GPT 数据生成 SP Flash Tool 格式的 scatter 文件
/// 对齐 C# 版：包含 PRELOADER 块、EMMC_BOOT_1/2 区域、无 {} 空行
pub fn generate_scatter_from_gpt(
    gpt_data: &[u8],
    output_file: &str,
) -> Result<Vec<(String, u64, u64, u32)>, String> {
    let base = match gpt_data.windows(8).position(|w| w == b"EFI PART") {
        Some(off) => off,
        None => return Err("未找到 GPT 签名 (EFI PART)".to_string()),
    };

    let num_part_entries = u32::from_le_bytes(gpt_data[base + 80..base + 84].try_into().unwrap());
    let part_entry_size = u32::from_le_bytes(gpt_data[base + 84..base + 88].try_into().unwrap());
    let part_entry_start_lba =
        u64::from_le_bytes(gpt_data[base + 72..base + 80].try_into().unwrap());

    let mut table_start = (part_entry_start_lba as usize) * 512;
    if table_start + 4 <= gpt_data.len()
        && gpt_data[table_start..table_start + 4]
            .iter()
            .all(|&b| b == 0)
    {
        table_start += 4;
    }

    let mut partitions = Vec::new();

    for i in 0..num_part_entries {
        let entry_offset = table_start + (i as usize) * (part_entry_size as usize);
        if entry_offset + part_entry_size as usize > gpt_data.len() {
            break;
        }

        let entry = &gpt_data[entry_offset..entry_offset + part_entry_size as usize];
        let unique_guid_zero = entry[16..32].iter().all(|&b| b == 0);
        if unique_guid_zero {
            break;
        }

        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let start = first_lba.saturating_mul(512);
        let size = (last_lba.saturating_sub(first_lba) + 1).saturating_mul(512);

        let name_utf16: Vec<u16> = (0..28)
            .map(|j| u16::from_le_bytes([entry[56 + j * 2], entry[56 + j * 2 + 1]]))
            .collect();
        let name = String::from_utf16_lossy(&name_utf16)
            .trim_end_matches('\0')
            .to_string();

        let region =
            if name.eq_ignore_ascii_case("preloader") || name.eq_ignore_ascii_case("BOOTLOADERS") {
                2 // EMMC_BOOT_1
            } else {
                1 // EMMC_USER
            };

        partitions.push((name.clone(), start, size, region));
    }

    if !partitions.is_empty() {
        let mut lines = vec![
            "PRELOADER 0x0".to_string(),
            "  partition_index: SYS0".to_string(),
            "  partition_name: PRELOADER".to_string(),
            "  file_name: preloader_k69v1_64_k419.bin".to_string(),
            "  is_download: true".to_string(),
            "  type: SV5_BL_BIN".to_string(),
            "  linear_start_addr: 0x0".to_string(),
            "  physical_start_addr: 0x0".to_string(),
            "  partition_size: 0x80000".to_string(),
            "  region: EMMC_BOOT_1".to_string(),
            "  storage: HW_STORAGE_EMMC".to_string(),
            "  boundary_check: true".to_string(),
            "  is_reserved: false".to_string(),
            "  operation_type: BOOTLOADERS".to_string(),
            "  is_upgradable: true".to_string(),
            "  empty_boot_needed: false".to_string(),
            "  reserve: 0x00".to_string(),
            "".to_string(),
        ];

        // GPT 分区块
        for (i, (name, start, size, region)) in partitions.iter().enumerate() {
            let idx = i + 1;
            let region_name = if *region == 2 {
                "EMMC_BOOT_1"
            } else {
                "EMMC_USER"
            };
            let ptype = if *name == "preloader" {
                "SV5_BL_BIN"
            } else {
                "NORMAL_ROM"
            };
            let op = if *name == "preloader" {
                "BOOTLOADERS"
            } else {
                "UPDATE"
            };

            lines.push(format!("{} 0x{:X}", name, start));
            lines.push(format!("  partition_index: SYS{}", idx));
            lines.push(format!("  partition_name: {}", name));
            lines.push(format!("  file_name: {}.bin", name.to_lowercase()));
            lines.push("  is_download: true".to_string());
            lines.push(format!("  type: {}", ptype));
            lines.push(format!("  linear_start_addr: 0x{:X}", start));
            lines.push(format!("  physical_start_addr: 0x{:X}", start));
            lines.push(format!("  partition_size: 0x{:X}", size));
            lines.push(format!("  region: {}", region_name));
            lines.push("  storage: HW_STORAGE_EMMC".to_string());
            lines.push("  boundary_check: true".to_string());
            lines.push("  is_reserved: false".to_string());
            lines.push(format!("  operation_type: {}", op));
            lines.push("  is_upgradable: true".to_string());
            lines.push("  empty_boot_needed: false".to_string());
            lines.push("  reserve: 0x00".to_string());
            lines.push("".to_string());
        }

        let content = lines.join("\n");
        std::fs::write(output_file, content)
            .map_err(|e| format!("写入 scatter 文件失败: {}", e))?;
    }

    Ok(partitions)
}

/// 从文件分析 GPT 分区表（不需要 USB 设备）
#[allow(dead_code)]
pub fn read_gpt_from_file(filename: &str) -> Result<(), String> {
    println!("分析 GPT 分区表: {}", filename);

    let gpt_data = std::fs::read(filename).map_err(|e| format!("读取 {} 失败: {}", filename, e))?;
    parse_gpt_from_data(&gpt_data)
}
