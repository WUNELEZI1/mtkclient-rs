//! 能力格（Capability lattice）。
//!
//! 每个 `Capability` 表示“系统当前具备的一项可执行能力”，而不是“某个动作做过”。
//! 能力之间存在**偏序**：具备强能力即隐含具备弱能力（见 [`Capability::implies`]）。
//!
//! 这个偏序是 Planner 正确性的关键：BROM 拿到 `BromControlXfer` 后，
//! 不必再规划一步“获取 BromRead”；DA 拿到 `DaPatched` 后不必再要 `DaLoaded`。

/// 单项能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// BROM 层控制传输（linecode / register access 通道可用）
    BromControlXfer,
    /// BROM 层内存读
    BromRead,
    /// BROM 层内存写
    BromWrite,
    /// 硬件加密引擎访问（SEJ / DXCC 等）
    CryptoAccess,
    /// DA 已加载并完成握手
    DaLoaded,
    /// DA 认证已绕过（签名校验不再阻塞）
    DaAuthBypass,
    /// XFlash（V5）协议可用
    DaXFlash,
    /// XML（V6）协议可用
    DaXml,
    /// DA 已被 patch（可跳过 SLA/DAA/SBC 校验）
    DaPatched,
    /// BROM 安全标志已被绕过（一次绕过可覆盖后续 DA 校验）
    SafetyBypassed,
    /// 分区表可读
    StorageRead,
    /// 分区可写
    StorageWrite,
    /// GPT 已解析（含未分区区域计算）
    GptParsed,
    /// RPMB 可访问
    RpmbAccess,
}

impl Capability {
    /// 能力的位序号（用于 [`CapabilitySet`]）。
    pub const fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// 偏序：`self` 是否隐含 `other`。
    ///
    /// 规则（与 docs/ 中的能力表一致）：
    /// - `BromControlXfer` → `BromRead`
    /// - `DaXFlash` / `DaXml` → `DaLoaded`
    /// - `DaPatched` → `DaLoaded` + `DaAuthBypass`
    /// - `SafetyBypassed` → `DaAuthBypass`（一次 BROM 绕过使后续 DA 校验不再阻塞）
    /// - `GptParsed` → `StorageRead`
    pub const fn implies(self, other: Capability) -> bool {
        use Capability::*;
        if matches!((self, other), (a, b) if a as u32 == b as u32) {
            return true;
        }
        matches!(
            (self, other),
            (BromControlXfer, BromRead)
                | (DaXFlash, DaLoaded)
                | (DaXml, DaLoaded)
                | (DaPatched, DaLoaded)
                | (DaPatched, DaAuthBypass)
                | (SafetyBypassed, DaAuthBypass)
                | (GptParsed, StorageRead)
        )
    }

    /// 人类可读名（用于决策日志）。
    pub const fn label(self) -> &'static str {
        use Capability::*;
        match self {
            BromControlXfer => "brom_control_xfer",
            BromRead => "brom_read",
            BromWrite => "brom_write",
            CryptoAccess => "crypto_access",
            DaLoaded => "da_loaded",
            DaAuthBypass => "da_auth_bypass",
            DaXFlash => "da_xflash",
            DaXml => "da_xml",
            DaPatched => "da_patched",
            SafetyBypassed => "safety_bypassed",
            StorageRead => "storage_read",
            StorageWrite => "storage_write",
            GptParsed => "gpt_parsed",
            RpmbAccess => "rpmb_access",
        }
    }

    /// 全部能力（按位序），供遍历与调试输出。
    pub const ALL: [Capability; 14] = {
        use Capability::*;
        [
            BromControlXfer,
            BromRead,
            BromWrite,
            CryptoAccess,
            DaLoaded,
            DaAuthBypass,
            DaXFlash,
            DaXml,
            DaPatched,
            SafetyBypassed,
            StorageRead,
            StorageWrite,
            GptParsed,
            RpmbAccess,
        ]
    };
}

/// 能力集合。自研 bitset，避免为 14 个位引入 bitflags 依赖。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    pub const EMPTY: CapabilitySet = CapabilitySet(0);

    pub const fn from(cap: Capability) -> Self {
        CapabilitySet(cap.bit())
    }

    /// 直接置位（不做偏序展开）。
    pub fn insert(&mut self, cap: Capability) {
        self.0 |= cap.bit();
    }

    pub fn with(mut self, cap: Capability) -> Self {
        self.insert(cap);
        self
    }

    pub fn union(self, other: Self) -> Self {
        CapabilitySet(self.0 | other.0)
    }

    /// 是否具备某能力（考虑偏序）。
    pub fn contains(self, cap: Capability) -> bool {
        Capability::ALL
            .iter()
            .filter(|c| self.0 & c.bit() != 0)
            .any(|c| c.implies(cap))
    }

    /// 是否直接置位（不考虑偏序），用于日志“实际获得”。
    pub fn has_direct(self, cap: Capability) -> bool {
        self.0 & cap.bit() != 0
    }

    /// 缺失的能力（`required` 中不被满足的部分）。
    pub fn missing_from(self, required: CapabilitySet) -> CapabilitySet {
        let mut out = CapabilitySet::EMPTY;
        for cap in Capability::ALL {
            if required.contains(cap) && !self.contains(cap) {
                out.insert(cap);
            }
        }
        out
    }

    /// 是否满足需求集。
    pub fn satisfies(self, required: CapabilitySet) -> bool {
        Capability::ALL
            .iter()
            .all(|c| !required.contains(*c) || self.contains(*c))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    /// 直接由位模式构造（供 `const` 注册表使用，不做偏序校验）。
    pub const fn from_bits(bits: u32) -> Self {
        CapabilitySet(bits)
    }

    /// 直接置位的名称列表（稳定顺序）。
    pub fn names(self) -> Vec<&'static str> {
        Capability::ALL
            .iter()
            .filter(|c| self.has_direct(**c))
            .map(|c| c.label())
            .collect()
    }

    /// 从名称列表构造（测试与外部资源包用）。
    pub fn from_names(names: &[&str]) -> Option<Self> {
        let mut set = CapabilitySet::EMPTY;
        for n in names {
            let cap = Capability::ALL.iter().find(|c| {
                c.label() == *n || format!("{:?}", c).to_lowercase() == n.to_lowercase()
            })?;
            set.insert(*cap);
        }
        Some(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;

    #[test]
    fn brom_control_implies_read() {
        let s = CapabilitySet::from(BromControlXfer);
        assert!(s.contains(BromRead));
        assert!(!s.contains(BromWrite));
        assert!(s.has_direct(BromControlXfer));
        assert!(!s.has_direct(BromRead));
    }

    #[test]
    fn da_patched_implies_loaded_and_bypass() {
        let s = CapabilitySet::from(DaPatched);
        assert!(s.contains(DaLoaded));
        assert!(s.contains(DaAuthBypass));
        assert!(!s.contains(DaXFlash));
    }

    #[test]
    fn safety_bypass_covers_auth() {
        assert!(CapabilitySet::from(SafetyBypassed).contains(DaAuthBypass));
    }

    #[test]
    fn missing_and_satisfies() {
        let required = CapabilitySet::EMPTY.with(StorageRead).with(GptParsed);
        let have = CapabilitySet::from(DaLoaded).with(StorageRead);
        let missing = have.missing_from(required);
        assert!(missing.has_direct(GptParsed));
        assert!(!missing.has_direct(StorageRead));
        assert!(!have.satisfies(required));
        assert!(have.with(GptParsed).satisfies(required));
    }

    #[test]
    fn names_roundtrip() {
        let s = CapabilitySet::from(DaPatched).with(StorageRead);
        let names = s.names();
        let back = CapabilitySet::from_names(&names).unwrap();
        assert_eq!(back.bits(), s.bits());
        assert!(CapabilitySet::from_names(&["not_a_cap"]).is_none());
    }
}
