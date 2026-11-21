/// Chaos fault injection config (`--sim-chaos`).
///
/// Probabilities are always expressed in **per-mille**; `0` disables that item. All default to 0
/// (most stable; behavior identical to plain simulation). A **fixed seed** drives a deterministic
/// PRNG, so the same seed reproduces the same fault sequence exactly — the precondition for chaos
/// to both "hit the limits" and "regress reproducibly".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChaosConfig {
    pub seed: u64,
    /// Bit-flip probability (‰) when a write lands: simulates flash bit decay / link noise.
    /// Note: the flip happens **after the checksum passed**, so only read-back verification/CRC
    /// can catch it — exactly the guard line ensuring "silent corruption must never pass as success".
    pub bitflip_permille: u32,
    /// Probability (‰) of truncating a read response: simulates a bad cable dropping bytes (short packet).
    pub drop_permille: u32,
    /// Device silence probability (‰): triggers a read timeout and **recovers only after reconnect** (simulates a loose cable).
    pub silence_permille: u32,
    /// Transient timeout probability (‰): a single read times out then recovers (simulates latency spikes / contact jitter).
    pub glitch_permille: u32,
}

impl Default for ChaosConfig {
    fn default() -> Self {
        Self {
            seed: 0x0C0F_FEE0,
            bitflip_permille: 0,
            drop_permille: 0,
            silence_permille: 0,
            glitch_permille: 0,
        }
    }
}

impl ChaosConfig {
    /// Recommended `--sim-chaos` tier: not aggressive, but enough to expose resume/verify defects.
    pub fn typical(seed: Option<u64>) -> Self {
        Self {
            seed: seed.unwrap_or(0x5EED_1234),
            bitflip_permille: 8,
            // Silence = cable unplugged, requires manual replug, so it is not an "auto-recoverable"
            // stress tier; use `fault=silent` or explicit config to test disconnects/resume.
            drop_permille: 8,
            silence_permille: 0,
            glitch_permille: 8,
        }
    }

    /// Whether this is equivalent to "inject no faults".
    pub fn is_noop(&self) -> bool {
        self.bitflip_permille == 0
            && self.drop_permille == 0
            && self.silence_permille == 0
            && self.glitch_permille == 0
    }
}

/// SplitMix64: deterministic PRNG (zero dependencies, consistent across platforms).
#[derive(Debug, Clone)]
pub(super) struct ChaosRng(u64);

impl ChaosRng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(super) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Hit with probability `p` (per-mille).
    pub(super) fn hit(&mut self, p: u32) -> bool {
        p > 0 && (self.next_u64() % 1000) < p as u64
    }
}

/// Chaos runtime state (config + PRNG + counters).
#[derive(Debug, Clone)]
pub(super) struct ChaosState {
    pub(super) cfg: ChaosConfig,
    pub(super) rng: ChaosRng,
    pub(super) bitflips: u64,
    pub(super) drops: u64,
    pub(super) silences: u64,
    pub(super) glitches: u64,
}

impl ChaosState {
    /// Bit-flip decision: on hit returns (byte index in payload, bit number).
    pub(super) fn dirty(&mut self, len: usize) -> Option<(usize, u8)> {
        if len == 0 || !self.rng.hit(self.cfg.bitflip_permille) {
            return None;
        }
        self.bitflips += 1;
        let idx = (self.rng.next_u64() as usize) % len;
        let bit = (self.rng.next_u64() & 7) as u8;
        Some((idx, bit))
    }
}
