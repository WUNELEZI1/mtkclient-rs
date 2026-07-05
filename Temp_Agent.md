# Temp Agent 记录

## 2026-07-05

### zadig-rs 按 mtkclient-rs 流程修复
- 修复 `zadig-rs` 设备检测：改为只枚举当前 `present` 的 USB/Ports 设备，避免把未插入设备或历史残留实例误判为当前 BROM 设备。
- 新增驱动类型识别：按 `mtkclient-rs` 逻辑区分 `WinUSB`、`Serial`、`Unknown`，检测到 `WinUSB` 时直接退出并提示无需重复安装。
- 修复串口流程：仅在检测到串口驱动时才执行 BROM 握手和关闭 watchdog，且握手或关闭 watchdog 失败会立即停止，不再继续安装驱动。
- 替换驱动安装流程：`zadig-rs` 改为使用 `wdi-rs prepare_driver + UpdateDriverForPlugAndPlayDevicesW + nusb 验证`，对齐 `mtkclient-rs` 的 WinUSB 安装路径，不再依赖旧的内嵌 `brom.inf` 自签名安装方式。
- 调整 `full` 流程：安装成功后直接结束，不再输出额外后续操作提示。

### 验证
- `cargo fmt --manifest-path d:\test\ZybClient\zadig-rs\Cargo.toml`
- `cargo build --manifest-path d:\test\ZybClient\zadig-rs\Cargo.toml`
- `cargo clippy --manifest-path d:\test\ZybClient\zadig-rs\Cargo.toml --all-targets --all-features`
- `cargo test --manifest-path d:\test\ZybClient\zadig-rs\Cargo.toml`
