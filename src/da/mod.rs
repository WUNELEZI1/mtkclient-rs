//! Download Agent system
//!
//! Mirrors the Python mtkclient Library/DA/ layout:
//! - `xflash` — XFlash protocol primitives, DA structs (DAXFlash), EMI extraction, diagnostics, IO
//! - `xml`    — XML (V6) DA protocol (MT6789+ platforms): command building + session + flash ops
//! - `engine` — protocol engine decoupled from StreamPort (for simulation / CI, stage-complete)
//! - `loader` — two-stage DA upload (header parsing + env init + upload)
//! - `ext`    — DA Extension commands (patch + generate + meta/adb control)
//!
//! DAXFlash is the core struct of the whole DA subsystem and is re-exported here.

pub mod engine;
pub mod ext;
#[cfg(test)]
pub mod fixture;
pub mod loader;
pub mod xflash;
pub mod xml;

// Core re-export: let external modules use it as crate::da::DAXFlash
pub use xflash::DAXFlash;
