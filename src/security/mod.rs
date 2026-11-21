//! Security configuration subsystem.
//!
//! Handles crypto signing, bootloader unlock and security config management.
//!
//! # Architecture
//! - `sej` - SEJ/HACC signing backend
//! - `seccfg` - security config parsing and patching (V3/V4)
//! - `vbmeta` - vbmeta disable and patch
//! - `frp` - FRP (Factory Reset Protection) unlock
//! - `obfuscate` - anti-reverse engineering: compile-time string obfuscation macro `obf!` and anti-debug probes

pub mod avb;
#[path = "frp.rs"]
pub mod frp;
#[path = "obfuscate.rs"]
pub mod obfuscate;
#[path = "seccfg/mod.rs"]
pub mod seccfg;
#[path = "sej.rs"]
pub mod sej;
#[path = "vbmeta.rs"]
pub mod vbmeta;

// Public API re-exports.
#[allow(unused_imports)]
pub use frp::frp_unlock;
#[allow(unused_imports)]
pub use seccfg::{seccfg_info, seccfg_info_file};
#[allow(unused_imports)]
pub use sej::sej_hacc_sign;
