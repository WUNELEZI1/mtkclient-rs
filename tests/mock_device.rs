//! Helpers for black-box integration tests against the built binary.

use std::process::{Command, Output};

/// Invoke the compiled `mtkclient-rs` binary with the given arguments.
pub fn run(args: &[&str]) -> Output {
    let cwd = std::env::temp_dir().join(format!("mtkclient-it-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("failed to create integration test workdir");
    Command::new(env!("CARGO_BIN_EXE_mtkclient-rs"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to spawn mtkclient-rs")
}

/// Combine stdout and stderr so assertions work regardless of the output stream.
pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
