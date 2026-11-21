//! Black-box integration tests: they only use the public CLI surface.

#[path = "../mock_device.rs"]
mod mock_device;

use mock_device::{run, text};

#[test]
fn help_uses_github_and_drops_qq() {
    let out = run(&["help"]);
    assert!(out.status.success(), "help failed: {}", text(&out));
    let s = text(&out);
    assert!(s.contains("github.com/WUNELEZI1/mtkclient-rs"), "{s}");
    assert!(s.contains("GPL-3.0-or-later"), "{s}");
    assert!(!s.contains("3535571067"), "QQ number still present: {s}");
    assert!(!s.contains("QQ:"), "QQ label still present: {s}");
    assert!(!s.contains("gitee.com/WUNELEZI1/mtkclient-rs"), "{s}");
}

#[test]
fn version_matches_manifest() {
    let out = run(&["--no_elevate", "--version"]);
    assert!(out.status.success(), "version failed: {}", text(&out));
    assert!(
        text(&out).contains(env!("CARGO_PKG_VERSION")),
        "{}",
        text(&out)
    );
}

#[test]
fn plan_emits_structured_decision_log() {
    let da = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/da_v5_6768.bin");
    let out = run(&["--no_elevate", "plan", "printgpt", "--da", da]);
    assert!(out.status.success(), "plan failed: {}", text(&out));
    let s = text(&out);
    assert!(s.contains("\"goal\""), "missing goal in decision log: {s}");
    assert!(s.contains("steps"), "missing steps in decision log: {s}");
}

#[test]
fn simulated_printgpt_uses_fixture_profile() {
    let profile = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/integration-clean.sim"
    );
    let out = run(&["--no_elevate", "--sim", profile, "printgpt"]);
    assert!(out.status.success(), "sim printgpt failed: {}", text(&out));
    let s = text(&out);
    assert!(s.contains("GPT 已解析"), "missing GPT summary: {s}");
    assert!(s.contains("boot"), "fixture partition missing: {s}");
}
