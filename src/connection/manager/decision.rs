//! Pure decision helpers for `smart_init`.
//!
//! Extracted from `manager.rs` to keep that file within the line-length limit
//! while keeping the decision logic free of side effects.

use super::{ConnectionManager, DeviceMode, RECONNECT_INTERVAL_MS, UsbStage};
use std::time::Duration;

#[cfg(target_os = "windows")]
use super::SERIAL_HANDSHAKE_RETRY_DELAY_SECS;

/// State written to the manager after a successful handshake.
///
/// This captures the repeated `mode` / `stage` / `port_name` assignment triplet:
/// the USB stage is derived purely from the device mode.
#[derive(Debug, PartialEq, Clone)]
pub(super) struct ConnectedState {
    pub(super) mode: DeviceMode,
    pub(super) stage: UsbStage,
    pub(super) port_name: Option<String>,
}

impl ConnectedState {
    /// Pure constructor: derive the stage from the mode and capture the port.
    pub(super) fn new(mode: DeviceMode, port_name: Option<String>) -> Self {
        let stage = match mode {
            DeviceMode::Brom => UsbStage::Brom,
            DeviceMode::Preloader => UsbStage::Preloader,
        };
        ConnectedState {
            mode,
            stage,
            port_name,
        }
    }

    /// Write this state onto the manager (the impure step executed by `smart_init`).
    pub(super) fn apply(self, manager: &mut ConnectionManager) {
        manager.mode = self.mode;
        manager.stage = self.stage;
        manager.port_name = self.port_name;
    }
}

/// Side-effect-free wait policy produced by the retry logic; `smart_init` only executes it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum SmartInitDecision {
    /// Wait the standard reconnect interval before the next detection iteration.
    ReconnectInterval,
    /// Wait the serial-handshake retry delay between two attempts (Windows only).
    #[cfg(target_os = "windows")]
    SerialRetryDelay,
}

impl SmartInitDecision {
    /// Pure mapping from a decision to its concrete sleep duration.
    pub(super) fn sleep_duration(self) -> Duration {
        match self {
            SmartInitDecision::ReconnectInterval => Duration::from_millis(RECONNECT_INTERVAL_MS),
            #[cfg(target_os = "windows")]
            SmartInitDecision::SerialRetryDelay => {
                Duration::from_secs(SERIAL_HANDSHAKE_RETRY_DELAY_SECS)
            }
        }
    }
}
