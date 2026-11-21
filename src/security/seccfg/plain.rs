//! Structured parsing of decrypted SecCfg V3 plaintext ("SW plaintext breakdown")
//!
//! Background: after decryption, the V3 encrypted section is 0x2C-aligned plaintext:
//!
//! ```text
//! +0x000  20 × 0x68  one imginfo each (name[32] + several u32 metadata fields)
//! +0x820  u32        siu_status
//! +0x824  u32        seccfg_status
//! +0x828  u32        seccfg_attr (the real source of the lock state)
//! +0x82C  ...        seccfg_ext (version-dependent extension fields)
//! ```
//!
//! The old implementation inferred hwtype only from "the first 4 decrypted bytes are IIII/CCCC/0000",
//! but all four decryption algorithms can accidentally hit `0000`, so hwtype could be misidentified.
//! This module instead **scores** the plaintext:
//! - the leading 4-byte marker (IIII / CCCC / all zero)
//! - whether the 20 imginfo name fields are readable (visible ASCII + NUL padding)
//! - how many known image names (boot / recovery / system ...) match
//! - whether the trailing attr falls into the known value set
//!
//! The scoring function depends only on the plaintext, so it can be unit-tested offline without a device.

/// Number of imginfo entries (fixed at 20, matching mtkclient)
pub(crate) const IMG_COUNT: usize = 20;
/// Size of a single imginfo entry
pub(crate) const IMG_ENTRY_SIZE: usize = 0x68;
/// Length of the imginfo name field
pub(crate) const IMG_NAME_LEN: usize = 32;
/// Length of the plaintext header (imginfo area)
pub(crate) const PLAIN_HEAD_LEN: usize = IMG_COUNT * IMG_ENTRY_SIZE;
/// Minimum plaintext length: header + the three u32 fields siu/status/attr
pub(crate) const PLAIN_MIN_LEN: usize = PLAIN_HEAD_LEN + 12;
/// `seccfg_ext` length (0x1004, matching mtkclient `ed.bytes(0x1000 + 4)`)
#[allow(dead_code)]
pub(crate) const EXT_LEN: usize = 0x1004;
/// A few platforms put the marker at 0x850 (mtkclient asks for samples; rewriting is not supported yet)
pub(crate) const ALT_MARKER_OFFSET: usize = 0x850;

/// The two valid `seccfg_status` values: complete / incomplete
pub(crate) const STATUS_COMPLETE: u32 = 0x4343_4343; // "CCCC"
pub(crate) const STATUS_INCOMPLETE: u32 = 0x4949_4949; // "IIII"

/// Type of the leading 4-byte marker in the plaintext
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlainMarker {
    /// `IIII`: normal image info section
    Img,
    /// `CCCC`: config section
    Cfg,
    /// All zero: some platforms omit the marker
    Zero,
    /// Anything else
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

/// Lock state (decoded from seccfg_attr at the end of the plaintext)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockState {
    Locked,
    Unlocked,
    /// Unknown value; the raw value is kept
    Unknown(u32),
}

impl LockState {
    pub(crate) fn from_attr(attr: u32) -> Self {
        match attr {
            0x4444_4444 => LockState::Unlocked,
            0x3333_3333 => LockState::Locked,
            // 0x6000..=0x6003 are the "locked" variants of different platform generations
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

/// One imginfo record (keeps the raw 0x68 bytes so write-back is byte-identical)
#[derive(Debug, Clone)]
pub(crate) struct ImgInfo {
    pub(crate) raw: [u8; IMG_ENTRY_SIZE],
    pub(crate) name: String,
    /// Metadata words after the name field (u32 LE), for troubleshooting
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

    /// Whether the name "looks like" a partition/image name: visible ASCII, contains letters, reasonable length
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

/// Known image names (a match significantly raises the score and helps tell the four decryption algorithms apart)
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

/// Structure of the decrypted plaintext
#[derive(Debug, Clone)]
pub(crate) struct Plain {
    /// Offset of the marker: normally 0, for the special layout 0x850
    pub(crate) marker_offset: usize,
    pub(crate) marker: PlainMarker,
    pub(crate) imgs: Vec<ImgInfo>,
    pub(crate) siu_status: u32,
    pub(crate) status: u32,
    pub(crate) attr: u32,
    /// Plaintext length (including the ext area)
    pub(crate) len: usize,
    /// Raw seccfg_ext bytes (preserved verbatim on write-back)
    pub(crate) ext: Vec<u8>,
    /// Score (0..=100); higher is more trustworthy
    pub(crate) score: u32,
}

impl Plain {
    /// Try to parse the plaintext; returns None if it is too short or the marker is unknown
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

    /// Locate the marker offset: prefer 0, then the special 0x850 layout
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

        // 1) leading 4-byte marker
        score += match self.marker {
            PlainMarker::Img | PlainMarker::Cfg => 30,
            PlainMarker::Zero => 12,
            PlainMarker::Other => 0,
        };

        // 2) imginfo name readability (up to 35 points)
        let plausible = self.imgs.iter().filter(|i| i.name_plausible()).count();
        score += ((plausible as u32) * 6).min(35);

        // 3) known image name hits (5 points each, up to 25)
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

        // 4) valid seccfg_status (only CCCC/IIII can be real plaintext)
        score += match self.status {
            STATUS_COMPLETE | STATUS_INCOMPLETE => 10,
            _ => 0,
        };

        // 5) valid lock-state value (up to 8 points)
        score += match LockState::from_attr(self.attr) {
            LockState::Locked | LockState::Unlocked => 8,
            LockState::Unknown(_) => 0,
        };

        // 6) penalty for the special 0x850 layout: readable, but rewriting this way is not recommended
        if self.marker_offset != 0 {
            score = score.saturating_sub(10);
        }

        score.min(100)
    }

    #[cfg(test)]
    pub(crate) fn lock_state(&self) -> LockState {
        LockState::from_attr(self.attr)
    }

    /// Non-empty image names (skipping marker blocks and empty entries), for log/info output
    pub(crate) fn non_empty_names(&self) -> Vec<&str> {
        self.imgs
            .iter()
            .filter(|i| !i.name.is_empty() && !matches!(i.name.as_str(), "IIII" | "CCCC" | "FFFF"))
            .map(|i| i.name.as_str())
            .collect()
    }
}

/// Pick the most trustworthy result from a set of candidate decryptions.
///
/// Arguments are `(hwtype, decrypted bytes)`; only candidates with score >= `min_score` are accepted.
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

    /// Build a synthetic plaintext with the given leading marker and image names
    fn synth(marker: &[u8; 4], names: &[&str], attr: u32) -> Vec<u8> {
        let mut buf = vec![0u8; PLAIN_MIN_LEN + EXT_LEN];
        buf[..4].copy_from_slice(marker);
        for (i, n) in names.iter().enumerate() {
            // Entry 0 is the marker block; image names start at entry 1 (matching the real layout)
            let off = (i + 1) * IMG_ENTRY_SIZE;
            let b = n.as_bytes();
            buf[off..off + b.len()].copy_from_slice(b);
            // Metadata: offset/length, mimicking the real structure
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
        // No valid status/attr set, so the score cannot beat good
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
