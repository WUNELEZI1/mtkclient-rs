use crate::da_xflash::DAXFlash;
use crate::sej::{sej_hacc_sign, with_backend};
use crate::da_xflash::{
    sej_sec_cfg_hw_decrypt,
    sej_sec_cfg_hw_encrypt,
    sej_sec_cfg_hw_v3_decrypt,
    sej_sec_cfg_hw_v3_encrypt,
    sej_sec_cfg_sw_decrypt,
    sej_sec_cfg_sw_encrypt,
};
use log::info;
use sha2::{Digest, Sha256};

// ==================== SecCfg V4 结构 ====================
// 对齐 Python seccfg.py:SecCfgV4
// V4 头部: magic(4) + seccfg_ver(4) + seccfg_size(4) + lock_state(4) +
//          critical_lock_state(4) + sboot_runtime(4) + endflag(4) = 28 字节
// 尾部: SHA256 hash(32 字节，经 AES 加密)

/// SecCfg V4 解析和修改
// 预留：unlock/lock 功能使用
pub(crate) struct SecCfgV4 {
    magic: u32,
    seccfg_ver: u32,
    seccfg_size: u32,
    lock_state: u32,
    critical_lock_state: u32,
    sboot_runtime: u32,
    endflag: u32,
    hwtype: String, // "SW", "V2", "V3", "V4"
}

impl SecCfgV4 {
    const MAGIC: u32 = 0x4D4D4D4D;
    const ENDFLAG: u32 = 0x45454545;

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

// ==================== SecCfg V3 结构 ====================
// 对齐 Python seccfg.py:SecCfgV3
// V3 头部: info_header(16) + magic(4) + seccfg_ver(4) + seccfg_size(4) +
//          seccfg_enc_offset(4) + seccfg_enc_len(4) + sw_sec_lock_try(1) +
//          sw_sec_lock_done(1) + page_size(2) + page_count(4) = 44 字节
// 加密数据段 + endflag(4)

// 预留：unlock/lock 功能使用
#[allow(dead_code)]
pub(crate) struct SecCfgV3 {
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
    imginfo: Vec<[u8; 0x68]>,
    seccfg_ext: Vec<u8>,
}

impl SecCfgV3 {
    const MAGIC: u32 = 0x4D4D4D4D;
    const ENDFLAG: u32 = 0x45454545;
    const ATTR_UNLOCK: u32 = 0x44444444;
    const ATTR_DEFAULT: u32 = 0x33333333;

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

    let output_path = if let Some(idx) = input_file.rfind('.') {
        format!("{}_unlock{}", &input_file[..idx], &input_file[idx..])
    } else {
        format!("{}_unlock", input_file)
    };

    std::fs::write(&output_path, &new_data).map_err(|e| format!("写入文件失败: {}", e))?;

    println!("  镜像格式为RAW ...");
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

fn build_v4_header(
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

fn build_v4_image_online(
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

/// 在线模式解锁 bootloader
/// 对齐刷机匣：读取 seccfg 分区，更新锁状态并重新签名后写回
pub fn unlock_bootloader(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始在线解锁 Bootloader...");
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let seccfg_data = da.readflash_data(seccfg_addr, seccfg_size)?;
    info!(
        "  读取 seccfg 分区: addr=0x{:X}, size={} 字节",
        seccfg_addr, seccfg_data.len()
    );

    let hw_code = da.preloader.get_hw_code().unwrap_or(0);
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        build_v4_image_online(da, &v4, "unlock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("unlock", seccfg_data.len())?
    };

    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("Bootloader 解锁成功");
    Ok(())
}

/// 在线模式锁定 bootloader
pub fn lock_bootloader(da: &mut DAXFlash) -> Result<(), String> {
    info!("开始在线锁定 Bootloader...");
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let seccfg_data = da.readflash_data(seccfg_addr, seccfg_size)?;

    let hw_code = da.preloader.get_hw_code().unwrap_or(0);
    let new_data = if seccfg_data.len() >= 28
        && u32::from_le_bytes(seccfg_data[0..4].try_into().unwrap()) == SecCfgV4::MAGIC
    {
        let v4 = SecCfgV4::parse(&seccfg_data)?;
        build_v4_image_online(da, &v4, "lock", seccfg_data.len(), hw_code)?
    } else {
        let v3 = SecCfgV3::parse(&seccfg_data)?;
        v3.create("lock", seccfg_data.len())?
    };

    da.write_flash_data(seccfg_addr, &new_data, 1, 8)?;
    info!("Bootloader 锁定成功");
    Ok(())
}
