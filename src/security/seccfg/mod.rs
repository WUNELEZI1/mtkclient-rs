//! SecCfg handling module (bootloader unlock/lock).
//!
//! Submodules:
//! - `v4`    - SecCfgV4 structure with parse/build (28-byte header + SHA256+AES signature)
//! - `v3`    - SecCfgV3 structure with parse/build (44-byte header + SEJ encrypted section)
//! - `build` - shared header build helpers (build_v4_header / sign_v4_digest)
//! - `plain` - structured parsing and scoring of decrypted V3 plaintext (to distinguish SW/V2/V3/V4)
//! - `cmd`   - top-level `seccfg info` read-only query entry
//!
//! Unlock/lock always goes through [`crate::ops::seccfg::SeccfgOp`], supporting all V3/V4 versions.

#[path = "build.rs"]
pub mod build;
#[path = "cmd.rs"]
pub mod cmd;
#[path = "info.rs"]
pub mod info;
#[path = "plain.rs"]
pub mod plain;
#[path = "v3.rs"]
pub mod v3;
#[path = "v4.rs"]
pub mod v4;

pub use cmd::{seccfg_info, seccfg_info_file};
