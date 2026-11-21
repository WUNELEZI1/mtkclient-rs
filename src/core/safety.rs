//! 安全标志位（来自 `GET_TARGET_CONFIG`，0xD8）。
//!
//! 位定义与 `docs/` 及 `system::config::TargetConfig` 保持一致：
//!
//! | bit | 名称 | 含义 |
//! |---|---|---|
//! | 0x001 | sbc | Secure Boot Control |
//! | 0x002 | sla | Serial Link Authentication |
//! | 0x004 | daa | Download Agent Authentication |
//! | 0x010 | cert | 需要根证书 |
//! | 0x020 | memread | 内存读取鉴权 |
//! | 0x040 | memwrite | 内存写入鉴权 |

/// 安全标志位集合。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SafetyFlags(u32);

impl SafetyFlags {
    pub const SBC: u32 = 0x001;
    pub const SLA: u32 = 0x002;
    pub const DAA: u32 = 0x004;
    pub const CERT: u32 = 0x010;
    pub const MEMREAD: u32 = 0x020;
    pub const MEMWRITE: u32 = 0x040;

    pub const NONE: SafetyFlags = SafetyFlags(0);

    pub const fn from_raw(raw: u32) -> Self {
        SafetyFlags(raw)
    }

    pub const fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    pub const fn sbc(self) -> bool {
        self.has(Self::SBC)
    }
    pub const fn sla(self) -> bool {
        self.has(Self::SLA)
    }
    pub const fn daa(self) -> bool {
        self.has(Self::DAA)
    }
    pub const fn cert(self) -> bool {
        self.has(Self::CERT)
    }
    pub const fn memread(self) -> bool {
        self.has(Self::MEMREAD)
    }
    pub const fn memwrite(self) -> bool {
        self.has(Self::MEMWRITE)
    }

    #[cfg(test)]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// 是否需要先获得 `DaPatched` / `SafetyBypassed` 才能加载 DA。
    ///
    /// 依据：SLA / DAA / CERT 任一置位时，未 patch 的 DA 会被拒绝；
    /// SBC 单独置位通常只影响镜像校验，不阻塞 DA 下载，因此不计入。
    pub const fn requires_da_bypass(self) -> bool {
        self.has(Self::SLA) || self.has(Self::DAA) || self.has(Self::CERT)
    }

    /// 人类可读列表（决策日志用）。
    pub fn names(self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.sbc() {
            v.push("sbc");
        }
        if self.sla() {
            v.push("sla");
        }
        if self.daa() {
            v.push("daa");
        }
        if self.cert() {
            v.push("cert");
        }
        if self.memread() {
            v.push("memread");
        }
        if self.memwrite() {
            v.push("memwrite");
        }
        v
    }
}

impl From<crate::system::config::TargetConfig> for SafetyFlags {
    fn from(t: crate::system::config::TargetConfig) -> Self {
        SafetyFlags(t.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_semantics() {
        let f = SafetyFlags::from_raw(SafetyFlags::SLA | SafetyFlags::DAA);
        assert!(f.sla() && f.daa());
        assert!(!f.sbc());
        assert!(f.requires_da_bypass());
        assert_eq!(f.names(), vec!["sla", "daa"]);
    }

    #[test]
    fn sbc_alone_does_not_require_bypass() {
        let f = SafetyFlags::from_raw(SafetyFlags::SBC);
        assert!(!f.requires_da_bypass());
        assert!(!f.is_zero());
    }

    #[test]
    fn none_is_zero() {
        assert!(SafetyFlags::NONE.is_zero());
        assert!(!SafetyFlags::NONE.requires_da_bypass());
    }
}
