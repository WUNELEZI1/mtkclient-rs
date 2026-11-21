//! SecCfg V3 结构（44 字节头 + SEJ 加密段 + 4 字节 endflag）
//!
//! 加密类型：SW / V2 / V3 / V4 四种 hwtype，对应不同 SEJ 加解密算法。

use log::{debug, info, warn};

use crate::security::sej::{
    sej_sec_cfg_hw_decrypt, sej_sec_cfg_hw_encrypt, sej_sec_cfg_hw_v3_decrypt,
    sej_sec_cfg_hw_v3_encrypt, sej_sec_cfg_sw_decrypt, sej_sec_cfg_sw_encrypt,
};

use super::plain::{self, Plain};

/// 明文最低可信分数：低于该值视为“解密失败/算法不匹配”
const MIN_PLAIN_SCORE: u32 = 40;

pub(crate) struct SecCfgV3 {
    pub(crate) info_header: [u8; 16],
    pub(crate) magic: u32,
    pub(crate) seccfg_ver: u32,
    pub(crate) seccfg_size: u32,
    pub(crate) seccfg_enc_offset: u32,
    pub(crate) seccfg_enc_len: u32,
    pub(crate) sw_sec_lock_try: u8,
    pub(crate) sw_sec_lock_done: u8,
    pub(crate) page_size: u16,
    pub(crate) page_count: u32,
    pub(crate) seccfg_attr: u32,
    pub(crate) seccfg_status: u32,
    /// SIU（安全镜像更新）状态位，回写时必须原样保留
    pub(crate) siu_status: u32,
    pub(crate) endflag: u32,
    pub(crate) hwtype: String,
    pub(crate) imginfo: Vec<[u8; 0x68]>,
    /// 解密后明文的结构化视图（名称/状态/锁状态），供 `seccfg info` 输出
    pub(crate) plain: Option<Plain>,
    /// 特殊 0x850 布局：可解析但拒绝改写
    pub(crate) alt_layout: bool,
    pub(crate) seccfg_ext: Vec<u8>,
}

/// 合成一张合法的 V3 seccfg（SW 加密、单组镜像名），供虚拟设备与单测共用。
/// `locked=true` 生成 ATTR_DEFAULT（锁定），false 生成 ATTR_UNLOCK。
pub(crate) fn synthetic_v3_image(locked: bool) -> Vec<u8> {
    let attr = if locked {
        SecCfgV3::ATTR_DEFAULT
    } else {
        SecCfgV3::ATTR_UNLOCK
    };
    let mut plain = vec![0u8; plain::PLAIN_HEAD_LEN];
    plain[..4].copy_from_slice(b"IIII");
    let names: &[&str] = &["preloader", "boot", "system"];
    for (i, n) in names.iter().enumerate() {
        let off = (i + 1) * plain::IMG_ENTRY_SIZE;
        plain[off..off + n.len()].copy_from_slice(n.as_bytes());
        plain[off + 32..off + 36].copy_from_slice(&0x1000u32.to_le_bytes());
    }
    plain.extend_from_slice(&0x1234u32.to_le_bytes()); // siu_status
    plain.extend_from_slice(&plain::STATUS_COMPLETE.to_le_bytes());
    plain.extend_from_slice(&attr.to_le_bytes());
    plain.extend(std::iter::repeat_n(0xA5u8, plain::EXT_LEN));

    let enc = sej_sec_cfg_sw_encrypt(&plain).expect("SW 加密不应失败");
    let size = 0x2C + enc.len() + 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"AND_SECCFG_v\0\0\0\0");
    out.extend_from_slice(&SecCfgV3::MAGIC.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // enc_offset
    out.extend_from_slice(&0x0100_0000u32.to_le_bytes()); // enc_len
    out.push(0); // sw_sec_lock_try
    out.push(0); // sw_sec_lock_done
    out.extend_from_slice(&0x200u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&enc);
    out.extend_from_slice(&SecCfgV3::ENDFLAG.to_le_bytes());
    while !out.len().is_multiple_of(0x200) {
        out.push(0);
    }
    out
}

impl SecCfgV3 {
    pub(crate) const MAGIC: u32 = 0x4D4D4D4D;
    pub(crate) const ENDFLAG: u32 = 0x45454545;
    pub(crate) const ATTR_UNLOCK: u32 = 0x44444444;
    pub(crate) const ATTR_DEFAULT: u32 = 0x33333333;

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

        // 加密段紧跟在 44 字节头之后，长度 = seccfg_size - 头(0x2C) - endflag(4)
        // （对齐 mtkclient: data = rrf.bytes(seccfg_size - 0x2C - 4)）
        const HEAD_LEN: usize = 0x2C;
        let body_len = (size as usize)
            .checked_sub(HEAD_LEN + 4)
            .ok_or_else(|| format!("seccfg V3 size 字段非法: {}", size))?;
        let body_end = HEAD_LEN + body_len;
        if body_len == 0 || body_end + 4 > data.len() {
            return Err(format!(
                "seccfg V3 加密数据段超出范围: size={}, 数据长度={}",
                size,
                data.len()
            ));
        }
        let enc_data = &data[HEAD_LEN..body_end];

        let endflag = u32::from_le_bytes(data[body_end..body_end + 4].try_into().unwrap());
        if endflag != Self::ENDFLAG {
            return Err("seccfg V3 endflag 不匹配".to_string());
        }

        // 逐一尝试四种解密算法，对结果“明文结构”打分后取最高分，
        // 避免旧实现“首个命中即采用”导致的 hwtype 误判。
        let mut candidates: Vec<(&'static str, Vec<u8>)> = Vec::with_capacity(4);
        if let Ok(d) = sej_sec_cfg_sw_decrypt(enc_data) {
            candidates.push(("SW", d));
        }
        if let Ok(d) = sej_sec_cfg_hw_v3_decrypt(enc_data, false) {
            candidates.push(("V2", d));
        }
        if let Ok(d) = sej_sec_cfg_hw_decrypt(enc_data) {
            candidates.push(("V3", d));
        }
        if let Ok(d) = sej_sec_cfg_hw_v3_decrypt(enc_data, true) {
            candidates.push(("V4", d));
        }

        let (hwtype, plain) = plain::best_plain(candidates, MIN_PLAIN_SCORE).ok_or_else(|| {
            "无法识别 seccfg V3 加密类型（四种解密结果均不像有效明文）".to_string()
        })?;

        debug!(
            "seccfg V3 明文细分: hwtype={}, marker={}, score={}, names={:?}",
            hwtype,
            plain.marker.label(),
            plain.score,
            plain.non_empty_names()
        );

        let imginfo: Vec<[u8; 0x68]> = plain.imgs.iter().map(|i| i.raw).collect();
        let seccfg_status = plain.status;
        let seccfg_attr = plain.attr;
        let siu_status = plain.siu_status;
        // seccfg_ext 必须原样保留，否则回写会把扩展区截断（旧实现固定写空 → 设备异常）
        let mut ext = plain.ext.clone();
        if plain.marker_offset != 0 {
            warn!("seccfg V3 使用 0x850 特殊布局，仅支持查看，禁止改写");
        }

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
            siu_status,
            endflag,
            hwtype,
            imginfo,
            alt_layout: plain.marker_offset != 0,
            seccfg_ext: std::mem::take(&mut ext),
            plain: Some(plain),
        })
    }

    pub(crate) fn create(&self, lockflag: &str, partition_size: usize) -> Result<Vec<u8>, String> {
        if self.alt_layout {
            return Err(
                "该设备 seccfg 使用 0x850 特殊布局，暂不支持自动改写（请用 'zyb seccfg info' 查看）"
                    .to_string(),
            );
        }
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
        inner.extend_from_slice(&self.siu_status.to_le_bytes());
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
        // size 字段 = 头 + 加密段 + endflag，保持与字节流一致
        let new_size = (0x2C + enc_data.len() + 4) as u32;
        result.extend_from_slice(&new_size.to_le_bytes());
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
            "seccfg V3 修改成功: attr=0x{:08X} -> 0x{:08X}, enc_len=0x{:08X} -> 0x{:08X}",
            self.seccfg_attr, new_attr, self.seccfg_enc_len, new_enc_len
        );
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::sej::sej_sec_cfg_sw_encrypt;

    /// 构造一张合法的 V3 seccfg（SW 加密），用于离线单测
    fn synth_v3(attr: u32, names: &[&str], ext_len: usize) -> Vec<u8> {
        let mut plain = vec![0u8; plain::PLAIN_HEAD_LEN];
        plain[..4].copy_from_slice(b"IIII");
        for (i, n) in names.iter().enumerate() {
            // 第 0 项是标记块，镜像名从第 1 项开始
            let off = (i + 1) * plain::IMG_ENTRY_SIZE;
            plain[off..off + n.len()].copy_from_slice(n.as_bytes());
            plain[off + 32..off + 36].copy_from_slice(&(0x1000u32).to_le_bytes());
        }
        plain.extend_from_slice(&0x1234u32.to_le_bytes()); // siu_status（回写需保留）
        plain.extend_from_slice(&plain::STATUS_COMPLETE.to_le_bytes());
        plain.extend_from_slice(&attr.to_le_bytes());
        plain.extend(std::iter::repeat_n(0xA5u8, ext_len));

        let enc = sej_sec_cfg_sw_encrypt(&plain).unwrap();
        let size = 0x2C + enc.len() + 4;
        let mut out = Vec::new();
        out.extend_from_slice(b"AND_SECCFG_v\0\0\0\0");
        out.extend_from_slice(&0x4D4D_4D4Du32.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // enc_offset
        out.extend_from_slice(&0x0100_0000u32.to_le_bytes()); // enc_len
        out.push(0); // sw_sec_lock_try
        out.push(0); // sw_sec_lock_done
        out.extend_from_slice(&0x200u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&enc);
        out.extend_from_slice(&0x4545_4545u32.to_le_bytes());
        while !out.len().is_multiple_of(0x200) {
            out.push(0);
        }
        out
    }

    #[test]
    fn parse_sw_plain_subdivision() {
        let img = synth_v3(0x3333_3333, &["preloader", "boot", "system"], 0x1004);
        let v3 = SecCfgV3::parse(&img).expect("解析应成功");
        assert_eq!(v3.hwtype, "SW");
        assert!(v3.seccfg_attr == 0x3333_3333);
        assert!(!v3.alt_layout);
        let p = v3.plain.as_ref().unwrap();
        assert_eq!(p.marker.label(), "IIII");
        assert!(p.score >= 75, "score={}", p.score);
        // 扩展区必须完整保留
        assert_eq!(v3.seccfg_ext.len(), 0x1004);
        assert!(v3.seccfg_ext.iter().all(|&b| b == 0xA5));
        assert_eq!(v3.siu_status, 0x1234);
        assert_eq!(p.non_empty_names(), vec!["preloader", "boot", "system"]);
    }

    #[test]
    fn unlock_preserves_ext_and_updates_attr() {
        let img = synth_v3(0x3333_3333, &["boot", "recovery"], 0x1004);
        let v3 = SecCfgV3::parse(&img).unwrap();
        let out = v3.create("unlock", img.len()).unwrap();

        let v3b = SecCfgV3::parse(&out).expect("回写结果应可再次解析");
        assert_eq!(v3b.seccfg_attr, SecCfgV3::ATTR_UNLOCK);
        assert_eq!(v3b.hwtype, "SW");
        // 扩展区不能被截断（旧实现固定写空 Vec）
        assert_eq!(v3b.seccfg_ext.len(), 0x1004);
        assert!(v3b.seccfg_ext.iter().all(|&b| b == 0xA5));
        // SIU 状态位必须原样保留（旧实现写 0，会清掉 SIU 更新标记）
        assert_eq!(v3b.siu_status, 0x1234);
    }

    #[test]
    fn lock_rejects_locked_attr() {
        // 已锁定的设备再次执行 lock 应直接报错（避免无意义重签）
        let img = synth_v3(0x3333_3333, &["boot"], 0x1004);
        let v3 = SecCfgV3::parse(&img).unwrap();
        assert!(
            v3.create("lock", img.len()).is_err(),
            "已锁定时不应重复上锁"
        );
    }

    #[test]
    fn info_report_roundtrip() {
        let img = synth_v3(0x4444_4444, &["boot", "system", "vendor"], 0x1004);
        let text = crate::security::seccfg::info::seccfg_report(&img).unwrap();
        assert!(text.contains("V3"));
        assert!(text.contains("已解锁"));
        assert!(text.contains("system"));
        assert!(text.contains("/100"));
    }

    #[test]
    fn unknown_format_reports_error() {
        assert!(SecCfgV3::parse(&[0u8; 64]).is_err());
        assert!(crate::security::seccfg::info::seccfg_report(&[0u8, 1, 2, 3]).is_err());
    }
}
