# Contributing / 贡献指南

## Quick start

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --bin mtkclient-rs
cargo test --test integration
cargo build --release
```

The development sandbox may lack an MSVC linker. In that case `cargo check --all-targets`, `clippy --all-targets` and `cargo test` still validate the code; the final `.exe` must be linked on a machine with the MSVC toolchain installed.

## Rules

- Target `x86_64-pc-windows-msvc`, compatible with Windows 10 x64.
- Architecture direction: `core ← transport ← da ← exec/ops ← cmd`. New features go through `ops` and capability checks, not ad-hoc command branches.
- No hardcoded chip or payload data: load them from `data/sdata.json`.
- Safety: never bypass `confirm::DangerConfirm` for destructive operations. Write paths must keep checksum verification and checkpoint support.
- Comments are short English; user-facing strings go through `i18n`.
- Every change: run the four checks above, then `git commit` and `git push`.

## Tests

- Unit tests live next to the code in `#[cfg(test)]` modules.
- Black-box CLI tests live in `tests/integration/`; shared helpers in `tests/mock_device.rs`; device fixtures in `tests/fixtures/`.
- Prefer `--sim <preset|fixture>` over real hardware for regression tests.

## License

By contributing you agree that your contribution is licensed under GPL-3.0-or-later.
