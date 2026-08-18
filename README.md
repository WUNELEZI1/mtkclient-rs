

```markdown
# MTKClient-RS

> 一个基于 Rust 开发的 MediaTek 芯片底层刷机工具，纯原生编译、单文件运行、零运行时依赖。
> 支持 87+ 种联发科芯片 | 适用于 Windows 10/11 x64
> 
> **交流群 (TG)**: https://t.me/mtkclient_rs  
> **作者**: sky&cfk99

---

## 中文

### 简介

**MTKClient-RS** 是一款使用 Rust 语言编写的底层刷机工具，专为 MediaTek（联发科）设备设计。它能够通过 USB 与设备的 BROM 或 Preloader 模式进行通信，实现对设备分区的底层读写操作、Bootloader 解锁、动态分区管理以及系统修复等功能。

该工具最大的特点是 **纯原生编译**，生成的单个 `.exe` 可执行文件无需安装 Python、.NET 或任何 VC++ 运行库，也不依赖外部的 libusb DLL，极大地方便了部署和使用。程序内置了 WinUSB 驱动安装模块，首次连接设备时无需手动使用 Zadig 等工具配置驱动。

### 核心特性

- **底层通信**: 支持 BROM 和 Preloader 两种连接模式，具备完善的断线重连与 DA (Download Agent) 会话复用机制。
- **分区操作**: 提供完整的分区读写 (`r`/`w`)、擦除 (`e`) 功能，支持 GPT 表解析与 `scatter.txt` 生成。
- **安全控制**: 支持通过 HACC 硬件加密引擎解锁/锁定 Bootloader (SECCFG V3/V4)，并提供 FRP 重置、AVB/vbmeta 修补等安全相关功能。
- **文件系统**: 自带 `fs_shell` 交互式 Shell，无需将镜像 pull 到本地即可直接在设备上浏览和修改 super 分区内的 ext4 文件系统。
- **高级功能**:
    - **内存读写**: `peek` / `poke` 命令直接操作 eMMC 内存地址。
    - **Payload 注入**: 支持通过 BROM Exploit 发送 Payload，提取 Preloader 或执行代码。
    - **断点续传**: 大文件读写支持 `Ctrl+C` 中断后从断点继续。
- **动态扩展**: 芯片支持列表及特定 Payload 通过 `data/sdata.json` 动态加载，无需频繁更新程序本体。

### 支持的芯片平台

该项目已验证兼容以下主流平台，理论上支持绝大多数搭载联发科芯片的设备（依据 `sdata.json` 动态列表）：

| 芯片型号 | 平台系列 | 状态 |
| :--- | :--- | :---: |
| MT6768 | Helio G85 / G88 | ✅ 支持 |
| MT6769 | Helio G85 / G88 系列 | ✅ 支持 |
| MT6771 | Helio P60 / P70 | ✅ 支持 |
| MT8183 | Helio P60/P70/G80 系 | ✅ 支持 |
| *其他 80+ 芯片* | - | ✅ 支持 |

> 完整支持列表请查阅源码中的 `data/sdata.json` 文件。

### 下载与安装

1. **获取工具**: 前往 [Releases](https://gitee.com/WUNELEZI1/mtkclient-rs/releases) 页面下载最新的预编译压缩包，并解压到任意目录（路径建议不含中文）。
2. **进入模式**: 设备完全关机，按住 **音量上键 + 下键** 并插入 USB 数据线进入 **BROM 模式**（若无法进入 BROM，可尝试仅按住音量上键进入 Preloader 模式）。
3. **运行程序**: 以 **管理员身份** 打开 PowerShell 或 CMD 终端。
4. **安装驱动**: 首次运行会自动检测并安装 WinUSB 驱动。如果自动安装失败，请进入 `drivers` 目录手动运行 `installer_x64.exe` 安装驱动。

### 快速开始

以下是一些常用操作的示例命令：

```powershell
# 1. 查看完整分区表 (会同时生成 scatter.txt)
.\mtkclient-rs.exe printgpt

# 2. 读取 boot_a 分区到本地文件
.\mtkclient-rs.exe r boot_a boot.img

# 3. 写入 boot_a 分区 (带自动校验)
.\mtkclient-rs.exe w boot_a boot.img

# 4. 擦除 lk_b 分区
.\mtkclient-rs.exe e lk_b

# 5. 解锁 Bootloader (通过 SECCFG HACC)
.\mtkclient-rs.exe zyb seccfg unlock

# 6. 交互式浏览设备文件系统 (类似 Linux Shell)
.\mtkclient-rs.exe fs_shell

# 7. 重启设备到 Fastboot 模式
.\mtkclient-rs.exe reboot fastboot
```

### 命令参考

| 命令 | 参数 | 说明 |
| :--- | :--- | :--- |
| `r` | `<分区名> [输出文件]` | 读取指定分区到本地文件 |
| `w` | `<分区名> <输入文件>` | 将本地文件写入指定分区 |
| `e` | `<分区名>` | 擦除指定分区 |
| `printgpt` | 无 | 打印分区表信息并生成 `scatter.txt` |
| `r gpt` | `[输出文件]` | 读取原始 GPT 头信息 |
| `reboot` | `[模式] --via <方式>` | 重启设备 (支持 system/fastboot/recovery/meta 等) |
| `zyb seccfg` | `unlock/lock` | 解锁/锁定 Bootloader (HACC) |
| `zyb oem` | `unlock/lock` | OEM 解锁/回锁 |
| `zyb frp` | `unlock/lock` | FRP (Factory Reset Protection) 解锁/回锁 |
| `zyb vbmeta` | `disable/enable` | 修补 vbmeta 以关闭/开启 AVB 校验 |
| `fs_shell` | 无 | 启动交互式 ext4 文件系统浏览器 |
| `dumppreloader` | 无 | 通过漏洞提取原始 Preloader 固件 |
| `peek` | `<地址> <大小>` | 读取内存数据 |
| `poke` | `<地址> <数据>` | 写入内存数据 |
| `slot` | `show` / `a` / `b` | 显示或切换 A/B 槽位 |
| `multi` | `<命令列表>` | 在单次 DA 会话中执行多条命令 |

**通用参数说明**:
- `--mode brom|preloader|auto`: 强制指定连接模式。
- `--da_x_speed <n>`: 设置 DA 传输速度等级 (数值越大越快但可能不稳定)。
- `--data-dir <路径>`: 指定 `data` 资源文件夹的路径。

### 数据目录结构

程序运行依赖 `data/` 目录下的配置文件和二进制文件，这些文件不包含在源码仓库的编译产物中，需要自行放置：

```
mtkclient-rs.exe
data/
├── sdata.json              # 芯片支持列表与 Payload 映射配置
├── MTK_DA_V5.bin           # 核心下载代理 DA (必须存在)
├── generic/                # 通用 Payload 目录
│   └── payload_xxx.bin
├── mt6768/                 # 特定芯片的 Payload 目录
│   └── mt6768_payload.bin
└── ... (其他芯片目录)
```

> **注意**: 如果您从源码构建 Release 版本，请务必将上述 `data` 目录完整复制到生成的 `mtkclient-rs.exe` 同级目录下，否则程序将无法找到 DA 和芯片配置而报错。

### 从源码构建

如果您想参与开发或自行编译，请确保已安装 Rust 工具链 (stable 版本)：

```bash
# 克隆仓库
git clone https://gitee.com/WUNELEZI1/mtkclient-rs.git
cd mtkclient-rs

# 编译 Release 版本
cargo build --release

# 编译产物位于 target/release/mtkclient-rs.exe
# 请按照上方说明放置 data 目录
```

### 安全与法律声明

- **用途限制**: 本工具仅供设备所有者在合法的前提下进行学习、研究、设备维修与数据备份使用。
- **操作风险**: 对设备进行写入 (`w`)、擦除 (`e`) 或解锁 (`unlock`) 等操作是 **不可逆** 的。请在操作前务必确保已备份重要数据，并仔细核对目标文件路径。
- **硬件保护**: 操作过程中 **请勿强制断开 USB 连接**，以免导致设备变砖。
- **合规性**: 请遵守您所在国家或地区的法律法规，勿将本工具用于任何未经授权的设备或恶意行为。

### 许可证

本项目采用 **Apache-2.0** 许可证开源。详情请查阅 [LICENSE](LICENSE) 文件。

---

## English

### Introduction

**MTKClient-RS** is a low-level flashing tool for MediaTek devices written in Rust. It supports reading, writing, and erasing device partitions, as well as advanced operations like Preloader/Bootloader unlocking, dynamic partition browsing, and Preloader extraction. The binary is **native and self-contained** — a single `.exe` with no dependency on Python, .NET, or libusb DLLs.

It communicates with the device's BROM or Preloader over USB (using WinUSB). It features DA (Download Agent) session reuse and resumable transfers to handle large partition operations efficiently.

### Features

- **Partition R/W/E**: `r`/`w`/`e` to manage any partition.
- **GPT Management**: `printgpt` dumps the full GPT and generates `scatter.txt`.
- **Bootloader Unlock**: `zyb seccfg unlock/lock` handles Bootloader locking via HACC (V3/V4); `zyb oem` and `frp` manage FRP/OEM state.
- **AVB / vbmeta**: `zyb vbmeta` patches vbmeta to disable/enable AVB verification.
- **Reboot Routing**: `reboot` supports `system`/`fastboot`/`recovery`/`meta` with `--via` selectors.
- **Interactive Filesystem**: `fs_shell` allows browsing the on-device super ext4 filesystem (`ls`/`cd`/`cat`/`cp`) without pulling images locally.
- **Preloader Extraction**: `dumppreloader` dumps Preloader firmware via BROM Exploit.
- **Memory R/W**: `peek`/`poke` read and write eMMC memory directly.
- **Session Reuse & Resume**: DA session persists across commands (`.state`); large transfers interrupted by `Ctrl+C` resume from the last checkpoint.
- **Auto Driver Install**: WinUSB driver is installed automatically on the first run via `wdi-rs`.
- **Dynamic Chip Loading**: Chips and payloads are resolved at runtime from `data/sdata.json`.

### Supported Chips

| Chip | Platform | Status |
| :--- | :--- | :---: |
| MT6768 | Helio G85 / G88 | ✅ Supported |
| MT6769 | Helio G85 / G88 family | ✅ Supported |
| MT6771 | Helio P60 / P70 | ✅ Supported |
| MT8183 | Helio P60/P70/G80 family | ✅ Supported |
| *80+ others* | - | ✅ Supported |

> Full list depends on `data/sdata.json`.

### Download & Installation

1. Download the latest archive from [Releases](https://gitee.com/WUNELEZI1/mtkclient-rs/releases) and extract it.
2. Power off the device, hold **Volume Up + Down** and plug in USB to enter **BROM mode**.
3. Open a terminal (PowerShell/CMD) **as Administrator**.
4. WinUSB is installed automatically on first run; if it fails, use `drivers/installer_x64.exe`.

### Quick Start

```powershell
# Print partition table
.\mtkclient-rs.exe printgpt

# Read boot_a
.\mtkclient-rs.exe r boot_a boot.img

# Write boot_a
.\mtkclient-rs.exe w boot_a boot.img

# Unlock Bootloader
.\mtkclient-rs.exe zyb seccfg unlock

# Interactive shell
.\mtkclient-rs.exe fs_shell
```

### Command Reference

| Command | Args | Description |
| :--- | :--- | :--- |
| `r` | `<part> [out]` | Read partition |
| `w` | `<part> <in>` | Write partition |
| `e` | `<part>` | Erase partition |
| `printgpt` | - | Print GPT, gen `scatter.txt` |
| `reboot` | `[mode] --via <w>` | Reboot device |
| `zyb seccfg` | `unlock/lock` | Unlock/Lock Bootloader |
| `fs_shell` | - | Interactive ext4 shell |
| `dumppreloader` | - | Dump Preloader |
| `peek` | `<addr> <size>` | Read memory |
| `poke` | `<addr> <data>` | Write memory |

### Data Directory Layout

Place `data/` (containing `MTK_DA_V5.bin`, `sdata.json`, and payloads) next to the executable.

### Build from Source

```bash
cargo build --release
```

### Security & Legal

See Chinese section for the full disclaimer. Use at your own risk. Licensed under Apache-2.0.
```