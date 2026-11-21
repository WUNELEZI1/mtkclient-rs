//! DA upload initialization module (entry point after split)
//!
//! Submodules:
//! - `header` — MTK AllInOne DA file parsing (parse_da_header / parse_da_regions / DaRegion)
//! - `env`    — XFlash protocol environment setup (setup_env / setup_hw_init / send_emi / boot_to)
//! - `upload` — Two-stage DA upload (upload_da1 / upload_da2)
//!
//! Note: this module lives at `crate::da::loader`,
//! migrated from the former `da_loader/` directory.

#[path = "env.rs"]
pub mod env;
#[path = "header.rs"]
pub mod header;
#[path = "upload.rs"]
pub mod upload;
