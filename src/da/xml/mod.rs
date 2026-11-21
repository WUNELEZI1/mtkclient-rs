//! XML (V6) DA protocol.
//!
//! Targets MT6789 and newer platforms: older platforms use XFlash ([`crate::da::xflash`]),
//! while newer DAs use XML messages. This module mirrors penumbra `core/src/da/xml/`:
//!
//! - [`cmd`]      — XML command builders (pure functions, no I/O)
//! - [`protocol`] — session layer for frame codec / ACK / lifecycle / data transfer
//! - [`flash`]    — partition read / write / erase / format over the XML protocol
//! - [`session`]  — high-level session facade for callers (composes the primitives above)

pub mod cmd;
pub mod flash;
pub mod protocol;
pub mod session;

pub use session::XmlSession;
