# BROM / Preloader 的 USB 识别

## 结论（以仓库现有实测代码为准，不引外部 GUID 表）

| 阶段 | VID | PID | 传输 |
|---|---|---|---|
| BROM | 0x0E8D | 0x0003 | WinUSB（Kamakiri2 需要 WinUSB/libusb） |
| Preloader | 0x0E8D | 0x2000 | 串口（主路径），WinUSB 兜底 |

证据：`src/usb/context.rs` 的 `UsbStage` 映射与 `src/preloader/transport/serial.rs:113-115`。

## 落地要求

- 枚举双路：`0x0E8D:0x0003` 走 WinUSB；`0x0E8D:0x2000` 走 serialport，同时尝试 WinUSB。
- 去重键用 `(VID, PID, SN)`；**COM 号会变，不做缓存键**。
- 不以 mtkclient 的 GUID 表为准：Windows 上驱动是否就位由 `EnumerateDiagnostics` 报告（设备管理器状态 + 实际枚举结果 + 期望 vs 实际），不假设 GUID。
- 未知 PID（如 0x2001、0x201C 等）先列出但不自动连接，提示用户确认。
