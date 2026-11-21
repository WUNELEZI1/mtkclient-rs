//! SecCfg V3 解密后明文的结构化解析（“SW 明文细分”）
//!
//! 背景：V3 的加密段解密后是一段 0x2C 对齐的明文：
//!
//! ```text
//! +0x000  20 × 0x68  每项 imginfo（name[32] + 若干 u32 元数据）
//! +0x820  u32        siu_status
//! +0x824  u32        seccfg_status
//! +0x828  u32        seccfg_attr（锁状态真正来源）
//! +0x82C  ...        seccfg_ext（版本相关的扩展字段）
//! ```
//!
//! 旧实现只用“解密结果前 4 字节是 IIII/CCCC/0000”来判断 hwtype，
//! 四种解密算法都可能偶然命中 `0000`，会把 hwtype 判错。
//! 本模块改为对明文做**打分**：
//! - 首 4 字节标记（IIII / CCCC / 全零）
//! - 20 个 imginfo 的名称字段是否可读（ASCII 可见字符 + NUL 填充）
//! - 已知镜像名字（boot / recovery / system ...）的命中数量
//! - 尾部 attr 是否落在已知取值集合内
//!
//! 打分函数只依赖明文本身，可离线单测，不需要设备。

/// imginfo 条目数量（固定 20 项，对齐 mtkclient）
pub(crate) const IMG_COUNT: usize = 20;
/// 单个 imginfo 条目大小
pub(crate) const IMG_ENTRY_SIZE: usize = 0x68;
/// imginfo 名称字段长度
pub(crate) const IMG_NAME_LEN: usize = 32;
/// 明文头部（imginfo 区）长度
pub(crate) const PLAIN_HEAD_LEN: usize = IMG_COUNT * IMG_ENTRY_SIZE;
/// 明文最短长度：头部 + siu/status/attr 三个 u32
pub(crate) const PLAIN_MIN_LEN: usize = PLAIN_HEAD_LEN + 12;
/// `seccfg_ext` 长度（0x1004，对齐 mtkclient `ed.bytes(0x1000 + 4)`）
#[allow(dead_code)]
pub(crate) const EXT_LEN: usize = 0x1004;
/// 少数平台把标记放在 0x850（mtkclient 会提示上报样本，暂不支持改写）
pub(crate) const ALT_MARKER_OFFSET: usize = 0x850;

/// `seccfg_status` 的两个合法取值：完整 / 不完整
pub(crate) const STATUS_COMPLETE: u32 = 0x4343_4343; // "CCCC"
pub(crate) const STATUS_INCOMPLETE: u32 = 0x4949_4949; // "IIII"

/// 明文首 4 字节标记类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlainMarker {
    /// `IIII`：普通镜像信息段
    Img,
    /// `CCCC`：配置段
    Cfg,
    /// 全零：部分平台不写标记
    Zero,
    /// 其它
    Other,
}

impl PlainMarker {
    pub(crate) fn of(head: &[u8]) -> Self {
        match head.get(..4) {
            Some(b"IIII") => PlainMarker::Img,
            Some(b"CCCC") => PlainMarker::Cfg,
            Some([0, 0, 0, 0]) => PlainMarker::Zero,
            _ => PlainMarker::Other,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            PlainMarker::Img => "IIII",
            PlainMarker::Cfg => "CCCC",
            PlainMarker::Zero => "0000",
            PlainMarker::Other => "????",
        }
    }
}

/// 锁状态（由明文尾部的 seccfg_attr 解码）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockState {
    Locked,
    Unlocked,
    /// 未知取值，保留原始值
    Unknown(u32),
}

impl LockState {
    pub(crate) fn from_attr(attr: u32) -> Self {
        match attr {
            0x4444_4444 => LockState::Unlocked,
            0x3333_3333 => LockState::Locked,
            // 0x6000..=0x6003 是各代平台的“已上锁”变体
            0x6000..=0x6003 => LockState::Locked,
            other => LockState::Unknown(other),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_locked(self) -> bool {
        matches!(self, LockState::Locked)
    }

    #[cfg(test)]
    pub(crate) fn is_unlocked(self) -> bool {
        matches!(self, LockState::Unlocked)
    }

    #[cfg(test)]
    pub(crate) fn label_zh(self) -> &'static str {
        match self {
            LockState::Locked => "已锁定",
            LockState::Unlocked => "已解锁",
            LockState::Unknown(_) => "未知",
        }
    }
}

/// 一条 imginfo 记录（保留原始 0x68 字节，保证回写时字节级一致）
#[derive(Debug, Clone)]
pub(crate) struct ImgInfo {
    pub(crate) raw: [u8; IMG_ENTRY_SIZE],
    pub(crate) name: String,
    /// 名称字段之后的元数据字（u32 LE），供排查用
    pub(crate) words: Vec<u32>,
}

impl ImgInfo {
    fn parse(raw: [u8; IMG_ENTRY_SIZE]) -> Self {
        let name_end = raw[..IMG_NAME_LEN]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(IMG_NAME_LEN);
        let name = String::from_utf8_lossy(&raw[..name_end]).to_string();
        let mut words = Vec::new();
        let mut off = IMG_NAME_LEN;
        while off + 4 <= IMG_ENTRY_SIZE {
            words.push(u32::from_le_bytes(raw[off..off + 4].try_into().unwrap()));
            off += 4;
        }
        ImgInfo { raw, name, words }
    }

    /// 名称是否“像”一个分区/镜像名：可见 ASCII、含字母、长度合理
    fn name_plausible(&self) -> bool {
        if self.name.is_empty() || self.name.len() > IMG_NAME_LEN {
            return false;
        }
        let mut has_alpha = false;
        for b in self.name.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' => has_alpha = true,
                b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b' ' => {}
                _ => return false,
            }
        }
        has_alpha
    }
}

/// 已知镜像名（命中可显著提高打分，帮助区分四种解密算法）
const KNOWN_IMAGES: &[&str] = &[
    "preloader",
    "boot",
    "recovery",
    "system",
    "vendor",
    "lk",
    "tee",
    "logo",
    "dtbo",
    "vbmeta",
    "super",
    "userdata",
    "md1img",
    "scp",
    "sspm",
    "spmfw",
    "gz",
    "mcupm",
    "pi_img",
    "cam_vpu1",
];

/// 解密后的明文结构
#[derive(Debug, Clone)]
pub(crate) struct Plain {
    /// 标记所在偏移：正常为 0，特殊布局为 0x850
    pub(crate) marker_offset: usize,
    pub(crate) marker: PlainMarker,
    pub(crate) imgs: Vec<ImgInfo>,
    pub(crate) siu_status: u32,
    pub(crate) status: u32,
    pub(crate) attr: u32,
    /// 明文长度（含 ext 区）
    pub(crate) len: usize,
    /// seccfg_ext 原始字节（回写时原样保留）
    pub(crate) ext: Vec<u8>,
    /// 打分（0..=100），越高越可信
    pub(crate) score: u32,
}

impl Plain {
    /// 尝试解析明文；结构长度不足或标记未知返回 None
    pub(crate) fn parse(dec: &[u8]) -> Option<Plain> {
        let base = Self::locate(dec)?;
        if dec.len() < base + PLAIN_MIN_LEN {
            return None;
        }
        let marker = PlainMarker::of(&dec[base..]);

        let mut imgs = Vec::with_capacity(IMG_COUNT);
        for i in 0..IMG_COUNT {
            let off = base + i * IMG_ENTRY_SIZE;
            let mut raw = [0u8; IMG_ENTRY_SIZE];
            raw.copy_from_slice(&dec[off..off + IMG_ENTRY_SIZE]);
            imgs.push(ImgInfo::parse(raw));
        }

        let tail = base + PLAIN_HEAD_LEN;
        let siu_status = u32::from_le_bytes(dec[tail..tail + 4].try_into().unwrap());
        let status = u32::from_le_bytes(dec[tail + 4..tail + 8].try_into().unwrap());
        let attr = u32::from_le_bytes(dec[tail + 8..tail + 12].try_into().unwrap());
        let ext = dec[tail + 12..].to_vec();

        let mut p = Plain {
            marker_offset: base,
            marker,
            imgs,
            siu_status,
            status,
            attr,
            len: dec.len(),
            ext,
            score: 0,
        };
        p.score = p.compute_score();
        Some(p)
    }

    /// 定位标记偏移：优先 0，其次 0x850 特殊布局
    fn locate(dec: &[u8]) -> Option<usize> {
        if dec.len() >= PLAIN_MIN_LEN && PlainMarker::of(dec) != PlainMarker::Other {
            return Some(0);
        }
        if dec.len() >= ALT_MARKER_OFFSET + 4
            && PlainMarker::of(&dec[ALT_MARKER_OFFSET..]) != PlainMarker::Other
        {
            return Some(ALT_MARKER_OFFSET);
        }
        None
    }

    fn compute_score(&self) -> u32 {
        let mut score: u32 = 0;

        // 1) 首 4 字节标记
        score += match self.marker {
            PlainMarker::Img | PlainMarker::Cfg => 30,
            PlainMarker::Zero => 12,
            PlainMarker::Other => 0,
        };

        // 2) imginfo 名称可读率（最多 35 分）
        let plausible = self.imgs.iter().filter(|i| i.name_plausible()).count();
        score += ((plausible as u32) * 6).min(35);

        // 3) 已知镜像名命中（每个 5 分，最多 25 分）
        let mut known = 0u32;
        for img in &self.imgs {
            if !img.name.is_empty()
                && KNOWN_IMAGES
                    .iter()
                    .any(|k| img.name.eq_ignore_ascii_case(k))
            {
                known += 1;
            }
        }
        score += (known * 5).min(25);

        // 4) seccfg_status 合法（CCCC/IIII 才可能是真实明文）
        score += match self.status {
            STATUS_COMPLETE | STATUS_INCOMPLETE => 10,
            _ => 0,
        };

        // 5) 锁状态取值合法（最多 8 分）
        score += match LockState::from_attr(self.attr) {
            LockState::Locked | LockState::Unlocked => 8,
            LockState::Unknown(_) => 0,
        };

        // 6) 特殊 0x850 布局扣分：能读但不建议按此改写
        if self.marker_offset != 0 {
            score = score.saturating_sub(10);
        }

        score.min(100)
    }

    #[cfg(test)]
    pub(crate) fn lock_state(&self) -> LockState {
        LockState::from_attr(self.attr)
    }

    /// 有效镜像名列表（跳过标记块与空项，用于日志/info 输出）
    pub(crate) fn non_empty_names(&self) -> Vec<&str> {
        self.imgs
            .iter()
            .filter(|i| !i.name.is_empty() && !matches!(i.name.as_str(), "IIII" | "CCCC" | "FFFF"))
            .map(|i| i.name.as_str())
            .collect()
    }
}

/// 从一组候选解密密文中挑选最可信的一个。
///
/// 参数为 `(hwtype, 解密结果)`；只有打分 >= `min_score` 的候选才会被接受。
pub(crate) fn best_plain(
    candidates: Vec<(&'static str, Vec<u8>)>,
    min_score: u32,
) -> Option<(String, Plain)> {
    let mut best: Option<(String, Plain)> = None;
    for (hwtype, dec) in candidates {
        let Some(p) = Plain::parse(&dec) else {
            continue;
        };
        if p.score < min_score {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, cur)) => p.score > cur.score,
        };
        if better {
            best = Some((hwtype.to_string(), p));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一段合成明文：指定前 4 字节标记与若干镜像名
    fn synth(marker: &[u8; 4], names: &[&str], attr: u32) -> Vec<u8> {
        let mut buf = vec![0u8; PLAIN_MIN_LEN + EXT_LEN];
        buf[..4].copy_from_slice(marker);
        for (i, n) in names.iter().enumerate() {
            // 第 0 项是标记块，镜像名从第 1 项开始（与真实布局一致）
            let off = (i + 1) * IMG_ENTRY_SIZE;
            let b = n.as_bytes();
            buf[off..off + b.len()].copy_from_slice(b);
            // 元数据：偏移/长度，模拟真实结构
            buf[off + IMG_NAME_LEN..off + IMG_NAME_LEN + 4]
                .copy_from_slice(&(0x1000u32 * (i as u32 + 1)).to_le_bytes());
            buf[off + IMG_NAME_LEN + 4..off + IMG_NAME_LEN + 8]
                .copy_from_slice(&(0x2000u32).to_le_bytes());
        }
        let base = PLAIN_HEAD_LEN;
        buf[base..base + 4].copy_from_slice(&0u32.to_le_bytes());
        buf[base + 4..base + 8].copy_from_slice(&STATUS_COMPLETE.to_le_bytes());
        buf[base + 8..base + 12].copy_from_slice(&attr.to_le_bytes());
        buf
    }

    #[test]
    fn marker_detection() {
        assert_eq!(PlainMarker::of(b"IIIIxxxx"), PlainMarker::Img);
        assert_eq!(PlainMarker::of(b"CCCCxxxx"), PlainMarker::Cfg);
        assert_eq!(PlainMarker::of(&[0, 0, 0, 0]), PlainMarker::Zero);
        assert_eq!(PlainMarker::of(b"ZZZZ"), PlainMarker::Other);
    }

    #[test]
    fn lock_state_from_attr() {
        assert!(LockState::from_attr(0x4444_4444).is_unlocked());
        assert!(LockState::from_attr(0x3333_3333).is_locked());
        assert!(LockState::from_attr(0x6003).is_locked());
        assert!(LockState::from_attr(0x6000).is_locked());
        assert_eq!(LockState::from_attr(0x1234).label_zh(), "未知");
    }

    #[test]
    fn parse_plain_with_known_images() {
        let data = synth(
            b"IIII",
            &["preloader", "boot", "recovery", "system"],
            0x4444_4444,
        );
        let p = Plain::parse(&data).expect("应能解析");
        assert_eq!(p.marker, PlainMarker::Img);
        assert!(p.lock_state().is_unlocked());
        assert!(p.score >= 70, "score={}", p.score);
        assert_eq!(p.non_empty_names().len(), 4);
        assert_eq!(p.imgs[1].name, "preloader");
    }

    #[test]
    fn garbage_is_rejected_or_low_score() {
        assert!(Plain::parse(&[0xAB; 32]).is_none(), "长度不足应拒绝");
        let mut junk = vec![0xABu8; PLAIN_MIN_LEN];
        junk[..4].copy_from_slice(b"IIII");
        let p = Plain::parse(&junk).unwrap();
        assert!(p.score < 50, "垃圾数据分数应偏低: {}", p.score);
    }

    #[test]
    fn best_plain_prefers_higher_score() {
        let good = synth(
            b"IIII",
            &["boot", "recovery", "system", "vendor"],
            0x3333_3333,
        );
        let mut bad = vec![0u8; PLAIN_MIN_LEN];
        bad[..4].copy_from_slice(&[0, 0, 0, 0]);
        // 未设置合法 status/attr，分数不足以胜过 good
        let picked = best_plain(vec![("SW", bad), ("V3", good)], 50).expect("应选中高分组");
        assert_eq!(picked.0, "V3");
        assert!(picked.1.lock_state().is_locked());
    }

    #[test]
    fn empty_names_do_not_boost_score() {
        let data = synth(b"0000", &[], 0x0);
        let p = Plain::parse(&data).unwrap();
        assert!(p.score < 40, "无有效名称时分数应低: {}", p.score);
        assert!(p.non_empty_names().is_empty());
    }
}
