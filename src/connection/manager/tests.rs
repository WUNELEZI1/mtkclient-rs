//! Table-driven unit tests for the pure `smart_init` decision helpers.

use super::decision::{ConnectedState, SmartInitDecision};
use super::{DeviceMode, RECONNECT_INTERVAL_MS, UsbStage};
use std::time::Duration;

#[cfg(target_os = "windows")]
use super::SERIAL_HANDSHAKE_RETRY_DELAY_SECS;

#[test]
fn connected_state_derives_stage_from_mode() {
    // (mode, input port, expected stage, expected retained port)
    let cases: Vec<(DeviceMode, Option<&str>, UsbStage, Option<&str>)> = vec![
        (DeviceMode::Brom, Some("COM3"), UsbStage::Brom, Some("COM3")),
        (
            DeviceMode::Preloader,
            Some("COM4"),
            UsbStage::Preloader,
            Some("COM4"),
        ),
        (DeviceMode::Brom, None, UsbStage::Brom, None),
        (DeviceMode::Preloader, None, UsbStage::Preloader, None),
    ];

    for (mode, port, expected_stage, expected_port) in cases {
        let state = ConnectedState::new(mode.clone(), port.map(str::to_string));
        assert_eq!(state.mode, mode);
        assert_eq!(state.stage, expected_stage);
        assert_eq!(state.port_name.as_deref(), expected_port);
    }
}

#[test]
fn smart_init_decision_sleep_duration() {
    // (decision, expected duration)
    let cases: Vec<(SmartInitDecision, Duration)> = vec![
        (
            SmartInitDecision::ReconnectInterval,
            Duration::from_millis(RECONNECT_INTERVAL_MS),
        ),
        #[cfg(target_os = "windows")]
        (
            SmartInitDecision::SerialRetryDelay,
            Duration::from_secs(SERIAL_HANDSHAKE_RETRY_DELAY_SECS),
        ),
    ];

    for (decision, expected) in cases {
        assert_eq!(decision.sleep_duration(), expected);
    }
}
