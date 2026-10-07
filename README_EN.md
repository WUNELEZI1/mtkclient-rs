# MTKClient-RS

> A native Rust toolkit for MediaTek device communication and flashing over BROM / Preloader, featuring DA session reuse, resumable transfers, dynamic chip support, and filesystem access.

[中文文档](README.md)

---

## Overview

**MTKClient-RS** is a native Rust implementation for low-level communication with MediaTek devices.

It is primarily designed for Windows and communicates directly with MediaTek devices through **USB / WinUSB**, supporting BROM, Preloader and DA-based operations.

The project focuses on providing a self-contained native toolchain while keeping device-specific information separate from the core implementation.

The Windows build is designed to run as a standalone executable:

* No Python runtime
* No .NET runtime
* No Visual C++ runtime
* No external libusb DLL
* Native Windows USB communication
* Automatic WinUSB driver handling

---

## Highlights

### Native Rust

The core is implemented in Rust and compiled natively for Windows.

The goal is to provide a lightweight executable that can be distributed without requiring a separate scripting runtime or large dependency stack.

### BROM / Preloader Communication

MTKClient-RS communicates with MediaTek devices through USB / WinUSB.

Supported connection stages include:

```text
BROM
Preloader
DA
```

Connection mode can be selected explicitly or detected automatically:

```text
--mode brom
--mode preloader
--mode auto
```

### DA Session Reuse

A major design feature is **DA session reuse**.

Instead of rebuilding the entire communication session for every operation, multiple commands can share the same DA session:

```text
USB
 ↓
BROM / Preloader
 ↓
DA
 ↓
Persistent Session
 ├── GPT
 ├── Partition I/O
 ├── Dynamic Partitions
 ├── Filesystem Access
 └── Reboot
```

This reduces unnecessary initialization and makes multi-step workflows significantly more efficient.

### Resumable Transfers

Large partition transfers can maintain persistent state and resume after interruption.

```text
large_partition.img
        ↓
     transfer
        ↓
    interrupted
        ↓
      .state
        ↓
      resume
```

This is particularly useful when working with large partitions such as `super` or `userdata`.

### Data-Driven Chip Support

Chip-specific information is intentionally separated from the core Rust implementation.

Runtime data is described through:

```text
data/sdata.json
```

This can contain:

* Supported chip definitions
* Payload mappings
* Chip-specific configuration
* Runtime parameters

The data-driven design makes it possible to extend device support without continuously adding hard-coded branches to the core implementation.

### Filesystem Shell

`fs_shell` provides interactive access to supported filesystems inside dynamic partitions.

Example:

```text
fs_shell

> ls
> cd system
> ls
> cat build.prop
> tree
> info
```

The goal is to inspect filesystem contents directly instead of requiring the entire partition image to be extracted first.

### Partition I/O

Basic partition operations are available through simple commands:

```powershell
.\mtkclient-rs.exe r boot_a boot_a.img
.\mtkclient-rs.exe w boot_a boot_a.img
.\mtkclient-rs.exe e boot_a
```

GPT information and dynamic partitions are also supported.

### Reboot and Device State

Common device targets include:

```text
system
fastboot
recovery
fastbootd
meta
```

Different reboot paths can be selected depending on the device state and communication channel.

### WinUSB Handling

The Windows build can handle WinUSB driver installation automatically on first use.

This is intended to reduce the amount of manual USB driver configuration required for end users.

---

## Feature Overview

| Feature                   | Status |
| ------------------------- | :----: |
| BROM communication        |    ✅   |
| Preloader communication   |    ✅   |
| DA communication          |    ✅   |
| GPT reading               |    ✅   |
| GPT / Scatter generation  |    ✅   |
| Partition read            |    ✅   |
| Partition write           |    ✅   |
| Partition erase           |    ✅   |
| Dynamic partition access  |    ✅   |
| DA session reuse          |    ✅   |
| Resumable transfers       |    ✅   |
| `fs_shell`                |    ✅   |
| ext4 filesystem access    |    ✅   |
| Preloader extraction      |    ✅   |
| Memory access             |    ✅   |
| A/B slot management       |    ✅   |
| Multi-command DA sessions |    ✅   |
| WinUSB handling           |    ✅   |
| Data-driven chip support  |    ✅   |

---

## Supported Platforms

Current project data includes support for multiple MediaTek platforms.

Examples include:

| SoC    | Platform               | Status |
| ------ | ---------------------- | :----: |
| MT6768 | Helio G85 / G88 series |    ✅   |
| MT6769 | Helio G85 / G88 series |    ✅   |
| MT6771 | Helio P60 / P70        |    ✅   |
| MT8183 | MediaTek platform      |    ✅   |
| MT8385 | MediaTek platform      |    ✅   |
| MT8666 | MediaTek platform      |    ✅   |

The authoritative support list is maintained in:

```text
data/sdata.json
```

Check the `support_chip` data for the exact runtime configuration.

---

## Getting Started

### 1. Download

Download the latest release from GitHub:

[GitHub Releases](https://github.com/WUNELEZI1/mtkclient-rs/releases?utm_source=chatgpt.com)

Extract the archive to any directory.

### 2. Connect a Device

Put the MediaTek device into the appropriate BROM or Preloader state and connect it to a Windows PC through USB.

### 3. Inspect GPT

```powershell
.\mtkclient-rs.exe printgpt
```

### 4. Read a Partition

```powershell
.\mtkclient-rs.exe r boot_a boot_a.img
```

### 5. Write a Partition

```powershell
.\mtkclient-rs.exe w boot_a boot_a.img
```

### 6. Erase a Partition

```powershell
.\mtkclient-rs.exe e boot_a
```

### 7. Browse the Filesystem

```powershell
.\mtkclient-rs.exe fs_shell
```

---

## Command Reference

| Command                  | Description                                   |
| ------------------------ | --------------------------------------------- |
| `r <partition> [output]` | Read a partition                              |
| `w <partition> <input>`  | Write a partition                             |
| `e <partition>`          | Erase a partition                             |
| `printgpt`               | Display GPT information                       |
| `r gpt`                  | Read raw GPT data                             |
| `reboot <mode>`          | Reboot the device                             |
| `fs_shell`               | Interactive filesystem access                 |
| `slot show`              | Show the current slot                         |
| `slot a/b`               | Switch A/B slot                               |
| `peek` / `poke`          | Memory access                                 |
| `dumppreloader`          | Extract Preloader                             |
| `multi`                  | Execute multiple operations in one DA session |
| `adb`                    | Enable ADB debugging in DA mode               |

Common options:

```text
--mode brom|preloader|auto
--da_x_speed <n>
--data-dir <path>
```

---

## Runtime Data

Runtime resources are stored under `data/`:

```text
mtkclient-rs.exe
data/
├── generic/
│   └── payload_xxx.bin
├── {chip}/
│   └── mtxxx.bin
├── MTK_DA_V5.bin
└── sdata.json
```

The directory contains:

* `generic/` — generic payloads
* `{chip}/` — chip-specific payloads
* `MTK_DA_V5.bin` — DA runtime component
* `sdata.json` — dynamic chip and payload mapping

The runtime data directory is intentionally separated from the Rust source tree.

---

## Building from Source

Requirements:

* Rust stable
* Windows x64

```powershell
git clone https://github.com/WUNELEZI1/mtkclient-rs.git
cd mtkclient-rs

cargo build --release
```

The resulting executable will be located at:

```text
target/release/mtkclient-rs.exe
```

Place the required `data/` directory next to the executable.

---

## Project Structure

```text
mtkclient-rs/
├── .github/
│   └── workflows/
├── src/
├── .cargo/
├── Cargo.toml
├── Cargo.lock
├── build.rs
├── flash_example.json
├── README.md
├── README_EN.md
└── LICENSE
```

---

## Design Goals

MTKClient-RS is not intended to be merely a line-by-line rewrite of an existing Python implementation.

The project focuses on several engineering goals:

### Native

Minimize runtime dependencies and make the Windows build easy to distribute.

### Data-driven

Keep chip definitions and payload information separate from the core implementation.

### Reusable

Reuse DA sessions across multiple operations instead of repeatedly rebuilding the communication stack.

### Resumable

Provide persistent state for large transfers so interrupted operations can be continued.

### Extensible

Keep communication, partition handling, filesystem access and device data sufficiently modular for future GUI applications and platform integrations.

---

## Safety and Legal Notice

This project is intended for device owners, developers and authorized repair or research environments.

Before performing destructive operations:

* Back up important data.
* Verify the target device and image files.
* Do not disconnect USB during active transfers.
* Only operate on devices you own or are authorized to service.
* Follow all applicable laws and regulations.

---

## License

Licensed under the **Apache License 2.0**.

See [`LICENSE`](LICENSE) for details.
