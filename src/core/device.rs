//! Device state: one of the Planner's inputs.
//!
//! Only fields **needed for planning** live here; no connection handles, no large buffers.

use super::safety::SafetyFlags;

/// DA protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    /// XFlash; the DA package header does not contain `MTK_DA_v6`
    V5,
    /// XML; the DA package header contains `MTK_DA_v6`
    V6,
    /// DA not parsed yet (`--loader` absent or failed to parse)
    Unknown,
}

impl ProtocolVersion {
    pub const fn label(self) -> &'static str {
        match self {
            ProtocolVersion::V5 => "V5(XFlash)",
            ProtocolVersion::V6 => "V6(XML)",
            ProtocolVersion::Unknown => "unknown",
        }
    }

    /// Whether the first 0x68 bytes of the DA file contain `MTK_DA_v6`.
    ///
    /// Based on `mtkclient/Library/DA/daconfig.py`: `v6 = b"MTK_DA_v6" in hdr`.
    /// This is the **authoritative source**; the chip table is only a low-confidence guess when no DA is provided.
    pub fn from_da_header(header: &[u8]) -> Self {
        if header.len() >= 0x68 {
            if header[..0x68].windows(9).any(|w| w == b"MTK_DA_v6") {
                return ProtocolVersion::V6;
            }
            return ProtocolVersion::V5;
        }
        if header.windows(9).any(|w| w == b"MTK_DA_v6") {
            ProtocolVersion::V6
        } else {
            ProtocolVersion::Unknown
        }
    }
}

/// V6 chips (DA codes) for which the README explicitly states "BootROM is patched; `--loader` required".
///
/// Source: the Chinese mtkclient README. **Not re-verified against the source tables**,
/// so the Planner treats it as a low-confidence guess and ORs it with the DA header result.
pub const KNOWN_V6_DA_CODES: [u16; 7] = [
    0x6781, // MT6781
    0x6789, // MT6789
    0x6855, // MT6855
    0x6886, // MT6886
    0x6895, // MT6895
    0x6983, // MT6983
    0x8985, // MT8985
];

/// Snapshot of the device state needed for planning.
#[derive(Debug, Clone)]
pub struct DeviceState {
    /// hw_code returned by BROM (usually something like 0x0707)
    pub hw_code: u16,
    /// da_code used inside the DA file (e.g. 0x6768)
    pub da_code: u16,
    /// Chip name (for display)
    pub name: String,
    /// Protocol version (from the DA header; authoritative)
    pub protocol: ProtocolVersion,
    /// Safety flags
    pub safety: SafetyFlags,
    /// BROM requires authentication (BROM path unreachable until auth is obtained)
    pub brom_auth_required: bool,
    /// Preloader already patched (DA signature verification fails)
    pub preloader_patched: bool,
    /// DA signature verification failed
    pub da_signature_failed: bool,
    /// Current active slot (A/B)
    pub current_slot: Option<char>,
    /// Whether affected by **CVE-2022-20069** (Preloader USB integer overflow causing an OOB write;
    /// physical access alone grants privilege escalation and bypasses SLA/DAA).
    ///
    /// Compatibility alias: early material mislabeled it as `CVE-2022-20435` (that one is an
    /// unauthenticated/EoP issue in an Android system service, unrelated to download mode).
    /// To keep existing callers working, the semantic alias `cve_20435_*` is kept
    /// (see the text parsing in `DeviceProfile`).
    pub cve_20069_affected: bool,
    /// Whether CVE-2022-20069 has been fixed by a vendor patch.
    pub cve_20069_patched: bool,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            hw_code: 0,
            da_code: 0,
            name: String::new(),
            protocol: ProtocolVersion::Unknown,
            safety: SafetyFlags::NONE,
            brom_auth_required: false,
            preloader_patched: false,
            da_signature_failed: false,
            current_slot: None,
            cve_20069_affected: false,
            cve_20069_patched: false,
        }
    }
}

impl DeviceState {
    /// Whether V6: the DA header wins; when unparsed, fall back to the chip list guess.
    pub fn is_v6(&self) -> bool {
        match self.protocol {
            ProtocolVersion::V6 => true,
            ProtocolVersion::V5 => false,
            ProtocolVersion::Unknown => KNOWN_V6_DA_CODES.contains(&self.da_code),
        }
    }

    /// Whether the BROM path is reachable.
    ///
    /// Three exclusions:
    /// 1. V6 chips (BROM exploit already patched);
    /// 2. BROM requires auth that has not been obtained yet;
    /// 3. Preloader patched and DA signature fails (no bypass left on the BROM side).
    pub fn brom_reachable(&self) -> bool {
        if self.is_v6() {
            return false;
        }
        if self.brom_auth_required {
            return false;
        }
        !(self.preloader_patched && self.da_signature_failed)
    }

    /// Whether signature checks must be bypassed before loading the DA.
    pub fn needs_da_bypass(&self) -> bool {
        self.safety.requires_da_bypass() || self.da_signature_failed
    }

    /// Short description (for the decision log).
    pub fn summary(&self) -> String {
        format!(
            "chip={}(0x{:04X}) proto={} safety=[{}] brom_reachable={}",
            if self.name.is_empty() {
                "unknown"
            } else {
                &self.name
            },
            self.da_code,
            self.protocol.label(),
            self.safety.names().join(","),
            self.brom_reachable()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da_header_detects_v6() {
        let mut hdr = vec![0u8; 0x100];
        hdr[0x40..0x49].copy_from_slice(b"MTK_DA_v6");
        assert_eq!(ProtocolVersion::from_da_header(&hdr), ProtocolVersion::V6);

        let hdr2 = vec![0xAAu8; 0x100];
        assert_eq!(ProtocolVersion::from_da_header(&hdr2), ProtocolVersion::V5);
        assert_eq!(
            ProtocolVersion::from_da_header(&[0u8; 8]),
            ProtocolVersion::Unknown
        );
    }

    #[test]
    fn v6_blocks_brom() {
        let mut s = DeviceState {
            da_code: 0x6983,
            ..Default::default()
        };
        assert!(s.is_v6());
        assert!(!s.brom_reachable());

        s.protocol = ProtocolVersion::V5;
        assert!(!s.is_v6(), "DA 头为 V5 时以 DA 为准，覆盖名单预判");
        assert!(s.brom_reachable());
    }

    #[test]
    fn auth_required_blocks_brom() {
        let s = DeviceState {
            protocol: ProtocolVersion::V5,
            brom_auth_required: true,
            ..Default::default()
        };
        assert!(!s.brom_reachable());
    }

    #[test]
    fn bypass_needed_on_safety_or_sig_fail() {
        let mut s = DeviceState::default();
        assert!(!s.needs_da_bypass());
        s.safety = SafetyFlags::from_raw(SafetyFlags::DAA);
        assert!(s.needs_da_bypass());
    }
}
