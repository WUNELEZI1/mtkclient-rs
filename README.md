# MTKClient-RS

> 一个基于 Rust 开发的 MediaTek 芯片底层刷机工具，纯原生编译、单文件运行、零运行时依赖。
> sky&cfk99

---

## 中文

### 简介

**MTKClient-RS** 是一款用 Rust 编写的 MediaTek（联发科）芯片底层刷机工具，支持对设备分区进行读取、写入、擦除，以及 Preloader/Bootloader 解锁、动态分区浏览、Preloader 提取等高级操作。程序为纯原生编译，**单个 `.exe` 即可运行，无需安装 Python / .NET / Visual C++ 运行库 / libusb DLL**。

程序运行时通过 USB（WinUSB）与设备的 BROM 或 Preloader 模式通信，支持 DA（Download Agent）会话复用与断点续传，可大幅缩短大分区读写的耗时。

### 特性

- **分区读写擦**：`r` / `w` / `e` 对任意分区进行读取、刷写、擦除。
- **GPT 管理**：`printgpt` 打印完整 GPT 分区表并生成 `scatter.txt`；`r gpt` 读取原始 GPT 数据。
- **Bootloader 解锁**：`zyb seccfg unlock/lock` 通过 seccfg 分区 HACC 硬件加密解锁/锁定 Bootloader（支持 V3/V4）；`zyb oem unlock/lock`、`frp` 用于 FRP/OEM 状态。
- **AVB / vbmeta**：`zyb vbmeta` 修补 vbmeta 以禁用/启用 AVB 验证。
- **重启路由**：`reboot` 支持 `system / fastboot / recovery / fastbootd / meta`，并可指定 `--via`（para/misc/da/xml/preloader）。
- **交互式文件系统**：`fs_shell` 直接在设备端浏览 super 内 ext4 文件系统（`ls` / `cd` / `cat` / `cp` / `tree` / `info`），无需先把镜像读到本地。
- **Preloader 提取**：`dumppreloader` 通过 BROM Exploit 发送 payload 提取 Preloader 固件。
- **内存读写**：`peek` / `poke` 直接读写 eMMC 内存。
- **会话复用 & 断点续传**：DA 会话跨命令复用（`.state`），大分区读写被 `Ctrl+C` 中断后可从断点继续。
- **驱动自动安装**：首次运行通过 `wdi-rs` 自动生成/签名/安装 WinUSB 驱动，无需 Zadig。
- **动态芯片加载**：通过 `data/sdata.json` 运行时动态解析芯片与对应 payload，不硬编码机型。

### 支持芯片

| 芯片 | 平台 | 状态 |
|------|------|------|
| MT6768 | Helio G85 / G88 | ✅ 已支持 |
| MT6769 | Helio G85 / G88 系列 | ✅ 已支持 |
| MT6771 | Helio P60 / P70 | ✅ 已支持 |
| MT8183 / MT8385 / MT8666 | Helio P60/P70/G80 系 | ✅ 已支持 |

此外通过 `sdata.json` 的动态芯片表支持 **6 种** MediaTek 芯片。具体以 `data/sdata.json` 中 `support_chip` 字段为准。

### 下载与安装

1. 从 [Releases](../../releases) 下载最新版压缩包并解压到任意目录。
2. 设备关机，按住 **音量上与下键** 插入 USB 进入 **BROM 模式**（或啥也不按进入 Preloader 模式）。
3. 右键以 **管理员身份** 打开终端（推荐PowerShell ）。
4. 首次运行会自动安装 WinUSB 驱动；若失败可用 `drivers/installer_x64.exe` 手动安装。

### 快速开始

```powershell
# 查看分区表（同时生成 scatter.txt）
.\mtkclient-rs.exe printgpt

# 读取 boot_a 分区到本地镜像
.\mtkclient-rs.exe r boot_a boot_a.img

# 写入 boot_a 分区
.\mtkclient-rs.exe w boot_a boot_a.img

# 擦除 lk_b 分区
.\mtkclient-rs.exe e lk_b

# 重启到 fastboot
.\mtkclient-rs.exe reboot fastboot

# 解锁 Bootloader（seccfg HACC）
.\mtkclient-rs.exe zyb seccfg unlock

# 交互式浏览 super 文件系统
.\mtkclient-rs.exe fs_shell
```

### 命令参考

| 命令 | 说明 |
|------|------|
| `r <分区> [输出文件]` | 读取分区到本地镜像 |
| `w <分区> <输入文件>` | 将本地镜像写入分区 |
| `e <分区>` | 擦除分区 |
| `printgpt` | 打印 GPT 分区表，生成 `scatter.txt` |
| `r gpt` | 读取原始 GPT 数据（MBR+GPT头+条目）|
| `reboot [模式] --via <方式>` | 重启设备（system/fastboot/recovery/fastbootd/meta）|
| `zyb seccfg unlock/lock` | 通过 HACC 解锁/锁定 Bootloader |
| `zyb oem unlock/lock` / `frp` | FRP/OEM 解锁与回锁 |
| `zyb vbmeta` | 修补 vbmeta 禁用/启用 AVB |
| `zyb erase_data` / `wipe_data` | 擦除 userdata 等，等效恢复出厂 |
| `zyb get_build_prop` | 直接从设备分区读取 `build.prop` |
| `slot show/a/b` | 显示/切换 A/B 槽位 |
| `multi` | 一次 DA 会话执行多条命令 |
| `adb` | DA 模式下启用 ADB 调试 |
| `fs_shell` | 交互式浏览 super 内 ext4 文件系统 |
| `dumppreloader` | 通过 BROM Exploit 提取 Preloader |
| `peek` / `poke` | 读写 eMMC 内存 |
| `r boot1/boot2/rpmb` | 读取特殊分区 |
| `r/w system/vendor/product/system_ext` | 读取/写入 super 内动态分区（自动匹配槽位）|

通用参数：`--mode brom|preloader|auto` 指定连接模式；`--da_x_speed <n>` 设置 DA 速度等级；`--data-dir <路径>` 指定数据目录。

### 数据目录结构

程序依赖 `data/` 目录下的运行时资源（payload、DA、芯片映射表），需与可执行文件放在同级目录：

```
mtkclient-rs.exe
data/
├── generic/
│   └── payload_xxx.bin        # 通用 payload
├── {chip}/                    # 各芯片专用 payload，如 mt6768/mt6768_payload.bin
│   └── mtxxx.bin
├── MTK_DA_V5.bin              # DA 下载代理（核心组件，不可删除）
└── sdata.json                 # 动态芯片映射表（support_chip / payloads）
```

> `data/` 为运行时活配置，不包含在源码仓库中；部署或切换 `--release` 构建时请自行放置。

### 从源码构建

```bash
# 需要 Rust 工具链（stable）
git clone https://gitee.com/WUNELEZI1/mtkclient-rs.git
cd mtkclient-rs
cargo build --release

# 产物位于 target/release/mtkclient-rs.exe
# 将 data/ 目录（generic/、各芯片 payload、MTK_DA_V5.bin、sdata.json）放到 exe 同级目录
```

### 安全与法律声明

- 本工具仅供 **设备所有者本人** 进行学习、研究、维修与备份之用。
- 写入、擦除、解锁等操作 **不可逆**，操作前请务必备份重要数据，并确保镜像文件正确。
- 操作过程中 **切勿断开 USB 连接**。
- 请遵守你所在国家/地区的法律法规，勿将本工具用于任何未授权设备。

### 许可证

[Apache-2.0](LICENSE)

---

## English

### Introduction

**MTKClient-RS** is a low-level MediaTek flash tool written in Rust. It can read, write and erase device partitions, and perform advanced operations such as Preloader/Bootloader unlocking, dynamic-partition browsing and Preloader extraction. The binary is **pure native — a single `.exe` with no Python / .NET / Visual C++ runtime / libusb DLL dependency**.

At runtime it talks to the device's BROM or Preloader over USB (WinUSB). DA (Download Agent) session reuse and resumable transfers dramatically shorten large partition operations.

### Features

- **Partition R/W/E**: `r` / `w` / `e` to read, flash and erase any partition.
- **GPT management**: `printgpt` dumps the full GPT and emits `scatter.txt`; `r gpt` reads raw GPT data.
- **Bootloader unlock**: `zyb seccfg unlock/lock` unlocks/locks the Bootloader via seccfg HACC (V3/V4); `zyb oem unlock/lock` and `frp` handle FRP/OEM state.
- **AVB / vbmeta**: `zyb vbmeta` patches vbmeta to disable/enable AVB verification.
- **Reboot routing**: `reboot` supports `system / fastboot / recovery / fastbootd / meta` with a `--via` selector (para/misc/da/xml/preloader).
- **Interactive filesystem**: `fs_shell` browses the on-device super ext4 filesystem (`ls` / `cd` / `cat` / `cp` / `tree` / `info`) without pulling the image locally.
- **Preloader extraction**: `dumppreloader` sends a payload via the BROM Exploit to dump the Preloader firmware.
- **Memory R/W**: `peek` / `poke` read and write eMMC memory directly.
- **Session reuse & resume**: DA session is reused across commands (`.state`); large transfers interrupted by `Ctrl+C` resume from the last checkpoint.
- **Auto driver install**: on first run, `wdi-rs` generates/signs/installs the WinUSB driver automatically — no Zadig needed.
- **Dynamic chip loading**: chips and their payloads are resolved at runtime from `data/sdata.json` — no hardcoded models.

### Supported Chips

| Chip | Platform | Status |
|------|----------|--------|
| MT6768 | Helio G85 / G88 | ✅ Supported |
| MT6769 | Helio G85 / G88 family | ✅ Supported |
| MT6771 | Helio P60 / P70 | ✅ Supported |
| MT8183 / MT8385 / MT8666 | Helio P60/P70/G80 family | ✅ Supported |

Additionally, **87+ MediaTek chips** are supported through the dynamic `sdata.json` table. See the `support_chip` field in `data/sdata.json` for the full list.

### Download & Installation

1. Download the latest archive from [Releases](../../releases) and extract it anywhere.
2. Power off the device, hold **Volume Down** and plug in USB to enter **BROM mode** (or Volume Up for Preloader mode).
3. Open a terminal (PowerShell / CMD) **as Administrator**.
4. WinUSB is installed automatically on first run; if it fails, use `drivers/installer_x64.exe`.

### Quick Start

```powershell
# Print the partition table (also generates scatter.txt)
.\mtkclient-rs.exe printgpt

# Read boot_a to a local image
.\mtkclient-rs.exe r boot_a boot_a.img

# Write boot_a from a local image
.\mtkclient-rs.exe w boot_a boot_a.img

# Erase lk_b
.\mtkclient-rs.exe e lk_b

# Reboot to fastboot
.\mtkclient-rs.exe reboot fastboot

# Unlock the Bootloader (seccfg HACC)
.\mtkclient-rs.exe zyb seccfg unlock

# Interactive super filesystem browser
.\mtkclient-rs.exe fs_shell
```

### Command Reference

| Command | Description |
|---------|-------------|
| `r <part> [out.img]` | Read a partition to a local image |
| `w <part> <in.img>` | Write a local image to a partition |
| `e <part>` | Erase a partition |
| `printgpt` | Print GPT, generate `scatter.txt` |
| `r gpt` | Read raw GPT data (MBR + GPT header + entries) |
| `reboot [mode] --via <way>` | Reboot device (system/fastboot/recovery/fastbootd/meta) |
| `zyb seccfg unlock/lock` | Unlock/lock Bootloader via HACC |
| `zyb oem unlock/lock` / `frp` | FRP/OEM unlock and relock |
| `zyb vbmeta` | Patch vbmeta to disable/enable AVB |
| `zyb erase_data` / `wipe_data` | Wipe userdata etc. (factory reset) |
| `zyb get_build_prop` | Read `build.prop` directly from device partitions |
| `slot show/a/b` | Show/switch A/B slot |
| `multi` | Run multiple commands in one DA session |
| `adb` | Enable ADB debugging in DA mode |
| `fs_shell` | Interactive ext4 browser inside super |
| `dumppreloader` | Dump Preloader via BROM Exploit |
| `peek` / `poke` | Read/write eMMC memory |
| `r boot1/boot2/rpmb` | Read special partitions |
| `r/w system/vendor/product/system_ext` | Read/write dynamic partitions in super (auto slot) |

Common flags: `--mode brom|preloader|auto` selects the connection mode; `--da_x_speed <n>` sets the DA speed grade; `--data-dir <path>` overrides the data directory.

### Data Directory Layout

The program relies on runtime assets under `data/` (payloads, DA, chip map). Place it next to the executable:

```
mtkclient-rs.exe
data/
├── generic/
│   └── payload_xxx.bin        # generic payload
├── {chip}/                    # per-chip payload, e.g. mt6768/mt6768_payload.bin
│   └── mtxxx.bin
├── MTK_DA_V5.bin              # DA download agent (core, do not delete)
└── sdata.json                 # dynamic chip map (support_chip / payloads)
```

> `data/` is runtime configuration and is **not** included in the source repository; deploy it (or copy it for `--release` builds) alongside the executable.

### Build from Source

```bash
# Requires the Rust stable toolchain
git clone https://gitee.com/WUNELEZI1/mtkclient-rs.git
cd mtkclient-rs
cargo build --release

# Output: target/release/mtkclient-rs.exe
# Place the data/ directory (generic/, per-chip payloads, MTK_DA_V5.bin, sdata.json) next to the exe
```

### Security & Legal

- This tool is intended for **device owners** for learning, research, repair and backup.
- Write / erase / unlock operations are **irreversible** — back up important data and verify images first.
- **Do not disconnect USB** during an operation.
- Comply with the laws and regulations in your jurisdiction; do not use this tool on any unauthorized device.

### License

[Apache-2.0](LICENSE)
