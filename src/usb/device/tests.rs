use super::*;

#[test]
fn reopen_retries_before_total_timeout() {
    assert!(reopen_should_retry(0));
    assert!(reopen_should_retry(REOPEN_POLL_INTERVAL_MS));
    assert!(reopen_should_retry(REOPEN_TIMEOUT_MS - 1));
}

#[test]
fn reopen_stops_at_total_timeout() {
    assert!(!reopen_should_retry(REOPEN_TIMEOUT_MS));
    assert!(!reopen_should_retry(REOPEN_TIMEOUT_MS + 1));
}

#[test]
fn reopen_constants_match_penumbra_policy() {
    // 200ms polling, 10s total timeout (matching penumbra reenumerate)
    assert_eq!(REOPEN_POLL_INTERVAL_MS, 200);
    assert_eq!(REOPEN_TIMEOUT_MS, 10_000);
}

// Compile-time check: the total timeout must allow at least 10 polls, otherwise polling degenerates to "wait once"
const _: () = assert!(REOPEN_TIMEOUT_MS / REOPEN_POLL_INTERVAL_MS >= 10);
