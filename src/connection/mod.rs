//! Connection management system
//!
//! Handles device connection, driver installation, and session management.
//!
//! # Architecture
//! - `driver` - Windows WinUSB driver installation/switching
//! - `manager` - Device connection manager (serial handshake / direct WinUSB / auto fallback)
//! - `session` - DA session state management

#[path = "driver/mod.rs"]
pub mod driver;
#[path = "manager.rs"]
pub mod manager;
pub mod reconnect;
#[path = "session.rs"]
pub mod session;

// Public API re-exports
#[allow(unused_imports)]
pub use manager::{ConnectionManager, DeviceMode};
// Note: try_reuse_da_session was removed along with the full disabling of DA session reuse and is no longer exported.
#[allow(unused_imports)]
pub use session::{SessionState, reset_session, save_da_session};
