//! Pure, device-independent image parsers shared by command handlers and ops.
//!
//! Modules here contain no command dispatch, no device I/O and no user-facing
//! output; they only turn raw image bytes into typed data. Keeping them below
//! the command layer lets the ops layer reuse them without importing `cmd`.

pub mod boot;
pub mod proinfo;
