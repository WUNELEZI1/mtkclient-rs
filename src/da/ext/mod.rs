//! DA extensions submodule.
//!
//! - `patches`  — DA1/DA2 shared and exclusive patches (binary search + apply + shared patch table)
//! - `generate` — generate_da_extensions (DA extensions template fill + wildcard search)
//! - `cmd`      — set_meta / enable_adb_and_reboot / peek / poke (DA mode control and memory access)
//!
//! All in `impl DAXFlash` blocks extending the `DAXFlash` struct.
//! The module-level public API (`patch_da1`/`patch_da2`/`find_binary`/`find_binary_wildcard`)
//! is for internal use by `impl DAXFlash` blocks only and is not re-exported (callers use methods).

#[path = "cmd.rs"]
pub mod cmd;
#[path = "generate.rs"]
pub mod generate;
#[path = "patch.rs"]
pub mod patch;
