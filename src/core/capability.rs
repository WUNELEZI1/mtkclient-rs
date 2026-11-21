//! Capability lattice.
//!
//! Each `Capability` represents "an executable ability the system currently has", not "an action that was performed".
//! Capabilities form a **partial order**: holding a strong capability implies holding weaker ones (see [`Capability::implies`]).
//!
//! This order is key to Planner correctness: once BROM has `BromControlXfer`,
//! it need not plan a separate "acquire BromRead" step; once DA has `DaPatched` it no longer needs `DaLoaded`.

/// A single capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// BROM control transfer (linecode / register access channel available)
    BromControlXfer,
    /// BROM memory read
    BromRead,
    /// BROM memory write
    BromWrite,
    /// Hardware crypto engine access (SEJ / DXCC, etc.)
    CryptoAccess,
    /// DA loaded and handshake completed
    DaLoaded,
    /// DA authentication bypassed (signature checks no longer block)
    DaAuthBypass,
    /// XFlash (V5) protocol available
    DaXFlash,
    /// XML (V6) protocol available
    DaXml,
    /// DA patched (SLA/DAA/SBC checks can be skipped)
    DaPatched,
    /// BROM security flags bypassed (one bypass covers later DA checks)
    SafetyBypassed,
    /// Partition table readable
    StorageRead,
    /// Partition writable
    StorageWrite,
    /// GPT parsed (including unpartitioned-region calculation)
    GptParsed,
    /// RPMB accessible
    RpmbAccess,
}

impl Capability {
    /// Bit index of the capability (used by [`CapabilitySet`]).
    pub const fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Partial order: whether `self` implies `other`.
    ///
    /// Rules (matching the capability table in docs/):
    /// - `BromControlXfer` → `BromRead`
    /// - `DaXFlash` / `DaXml` → `DaLoaded`
    /// - `DaPatched` → `DaLoaded` + `DaAuthBypass`
    /// - `SafetyBypassed` → `DaAuthBypass` (one BROM bypass unblocks later DA checks)
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

    /// Human-readable name (used in decision logs).
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

    /// All capabilities in bit order, for iteration and debug output.
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

/// Capability set. A hand-rolled bitset to avoid pulling in bitflags for 14 bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    pub const EMPTY: CapabilitySet = CapabilitySet(0);

    pub const fn from(cap: Capability) -> Self {
        CapabilitySet(cap.bit())
    }

    /// Set the bit directly (no partial-order expansion).
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

    /// Whether a capability is held (taking the partial order into account).
    pub fn contains(self, cap: Capability) -> bool {
        Capability::ALL
            .iter()
            .filter(|c| self.0 & c.bit() != 0)
            .any(|c| c.implies(cap))
    }

    /// Whether the bit is set directly (ignoring the partial order), for "actually acquired" logs.
    pub fn has_direct(self, cap: Capability) -> bool {
        self.0 & cap.bit() != 0
    }

    /// Missing capabilities (the parts of `required` that are not satisfied).
    pub fn missing_from(self, required: CapabilitySet) -> CapabilitySet {
        let mut out = CapabilitySet::EMPTY;
        for cap in Capability::ALL {
            if required.contains(cap) && !self.contains(cap) {
                out.insert(cap);
            }
        }
        out
    }

    /// Whether the requirement set is satisfied.
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

    /// Build directly from a bit pattern (for `const` registries, no partial-order validation).
    pub const fn from_bits(bits: u32) -> Self {
        CapabilitySet(bits)
    }

    /// Names of directly set bits (stable order).
    pub fn names(self) -> Vec<&'static str> {
        Capability::ALL
            .iter()
            .filter(|c| self.has_direct(**c))
            .map(|c| c.label())
            .collect()
    }

    #[cfg(test)]
    /// Build from a name list (for tests and external resource packs).
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
