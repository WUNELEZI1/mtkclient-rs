# MTKClient-RS

> 基于 Rust 构建的原生 MediaTek 设备通信与刷写工具，面向 BROM / Preloader，支持 DA 会话复用、断点续传、动态芯片支持与设备端文件系统访问。

[English](README_EN.md)

---

## 简介

**MTKClient-RS** 是一个使用 Rust 编写的 MediaTek（联发科）底层设备通信与刷写工具。

项目主要面向 Windows，直接通过 **USB / WinUSB** 与 MediaTek 设备的 **BROM / Preloader** 通信，并在 DA（Download Agent）阶段提供分区操作、动态分区访问、文件系统浏览以及设备状态管理等能力。

与传统依赖 Python 运行环境的工具不同，MTKClient-RS 的 Windows 构建版本采用原生编译方式：

* 单个 `.exe` 即可运行
* 无需安装 Python
* 无需 .NET Runtime
* 无需 Visual C++ Runtime
* 无需额外的 libusb DLL
* 首次运行可自动处理 WinUSB 驱动

项目同时采用**数据驱动的芯片支持模型**，将芯片映射、payload 等运行时信息从核心逻辑中分离，通过 `sdata.json` 进行扩展。

## 核心特性

### ⚡ 原生 Rust

核心代码使用 Rust 实现，并针对 Windows USB 环境进行原生编译。

相比依赖解释器或大型运行时环境的工具，更适合制作独立的 Windows 工具和上层应用后端。

### 🔌 BROM / Preloader 通信

通过 USB / WinUSB 与 MediaTek 设备通信，支持：

* BROM
* Preloader
* DA

并提供自动模式选择：

```text
--mode brom
--mode preloader
--mode auto
```

### 🚀 DA Session 复用

MTKClient-RS 支持在多个命令之间复用 DA 会话。

例如：

```text
连接设备
   ↓
BROM / Preloader
   ↓
DA
   ↓
保持 DA Session
   ├── GPT
   ├── Partition I/O
   ├── Dynamic Partition
   ├── Filesystem
   └── Reboot
```

避免每执行一个操作都重新建立完整的设备通信流程。

### ⏯️ 断点续传

对于大分区读写操作，支持保存状态并在中断后继续执行。

例如：

```text
large_partition.img
        ↓
   ┌───────────────┐
   │   transfer    │
   └───────────────┘
          ↓
       Ctrl+C
          ↓
      .state
          ↓
     resume later
```

这对于大容量 `super`、`userdata` 等分区尤其有用。

### 🧩 数据驱动的芯片支持

芯片相关信息并不全部硬编码在 Rust 源码中。

运行时数据由：

```text
data/sdata.json
```

负责描述，例如：

* 支持的芯片
* payload
* 芯片映射
* 相关运行时参数

因此，在扩展支持范围时，可以尽可能减少对核心逻辑的修改。

### 📂 文件系统访问

提供 `fs_shell`，可以直接访问设备上的动态分区文件系统。

例如：

```text
fs_shell

> ls
> cd system
> ls
> cat build.prop
> tree
> info
```

支持直接浏览 super 中的 ext4 文件系统，而不需要首先将完整镜像导出到本地。

### 💾 分区操作

支持常见的分区读取、写入和擦除：

```powershell
.\mtkclient-rs.exe r boot_a boot_a.img
.\mtkclient-rs.exe w boot_a boot_a.img
.\mtkclient-rs.exe e boot_a
```

同时支持 GPT 信息读取以及动态分区访问。

### 🔄 设备状态与重启

支持多个常见启动模式：

```text
system
fastboot
recovery
fastbootd
meta
```

并允许根据不同通信路径执行重启操作。

### 🛠️ WinUSB 驱动处理

Windows 首次使用时可以自动处理 WinUSB 驱动。

目标是让普通用户不需要首先安装复杂的 USB 开发环境或手动配置驱动。

---

## 功能概览

| 功能                 |  支持 |
| ------------------ | :-: |
| BROM 通信            |  ✅  |
| Preloader 通信       |  ✅  |
| DA 通信              |  ✅  |
| GPT 读取             |  ✅  |
| GPT / Scatter 信息生成 |  ✅  |
| 分区读取               |  ✅  |
| 分区写入               |  ✅  |
| 分区擦除               |  ✅  |
| 动态分区访问             |  ✅  |
| DA Session 复用      |  ✅  |
| 断点续传               |  ✅  |
| `fs_shell`         |  ✅  |
| ext4 文件系统访问        |  ✅  |
| Preloader 提取       |  ✅  |
| 内存读写               |  ✅  |
| A/B Slot 管理        |  ✅  |
| 多命令 DA Session     |  ✅  |
| WinUSB 自动处理        |  ✅  |
| 数据驱动芯片支持           |  ✅  |

---

## 支持的芯片

当前项目包含多个 MediaTek 平台的数据和 payload 支持。

已验证 / 已配置的平台包括：

| SoC    | 平台                 |  状态 |
| ------ | ------------------ | :-: |
| MT6768 | Helio G85 / G88 系列 |  ✅  |
| MT6769 | Helio G85 / G88 系列 |  ✅  |
| MT6771 | Helio P60 / P70    |  ✅  |
| MT8183 | MediaTek 平台        |  ✅  |
| MT8385 | MediaTek 平台        |  ✅  |
| MT8666 | MediaTek 平台        |  ✅  |

实际支持范围以：

```text
data/sdata.json
```

中的 `support_chip` 数据为准。

项目采用动态数据结构，因此支持列表可以独立于核心 Rust 代码进行扩展。

---

## 快速开始

### 1. 下载

从 GitHub Releases 下载对应版本并解压。

[GitHub Releases](https://github.com/WUNELEZI1/mtkclient-rs/releases?utm_source=chatgpt.com)

### 2. 连接设备

根据设备平台进入 BROM / Preloader 模式，然后通过 USB 连接 Windows PC。

### 3. 查看 GPT

```powershell
.\mtkclient-rs.exe printgpt
```

### 4. 读取分区

```powershell
.\mtkclient-rs.exe r boot_a boot_a.img
```

### 5. 写入分区

```powershell
.\mtkclient-rs.exe w boot_a boot_a.img
```

### 6. 擦除分区

```powershell
.\mtkclient-rs.exe e boot_a
```

### 7. 浏览文件系统

```powershell
.\mtkclient-rs.exe fs_shell
```

---

## 常用命令

| 命令                       | 说明                     |
| ------------------------ | ---------------------- |
| `r <partition> [output]` | 读取分区                   |
| `w <partition> <input>`  | 写入分区                   |
| `e <partition>`          | 擦除分区                   |
| `printgpt`               | 显示 GPT                 |
| `r gpt`                  | 读取原始 GPT 数据            |
| `reboot <mode>`          | 重启设备                   |
| `fs_shell`               | 交互式文件系统访问              |
| `slot show`              | 查看当前 Slot              |
| `slot a/b`               | 切换 Slot                |
| `peek` / `poke`          | 内存访问                   |
| `dumppreloader`          | 提取 Preloader           |
| `multi`                  | 在同一 DA Session 中执行多个操作 |
| `adb`                    | DA 环境下启用 ADB 调试        |

通用参数：

```text
--mode brom|preloader|auto
--da_x_speed <n>
--data-dir <path>
```

---

## 数据目录

MTKClient-RS 的运行时资源位于 `data/`：

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

其中：

* `generic/`：通用 payload
* `{chip}/`：芯片相关 payload
* `MTK_DA_V5.bin`：DA 运行时组件
* `sdata.json`：动态芯片与 payload 映射

`data/` 是运行时资源目录，不要求与核心 Rust 源码绑定。

---

## 从源码构建

需要：

* Rust stable
* Windows x64

```powershell
git clone https://github.com/WUNELEZI1/mtkclient-rs.git
cd mtkclient-rs

cargo build --release
```

生成文件：

```text
target/release/mtkclient-rs.exe
```

构建完成后，将对应的 `data/` 目录放置到可执行文件旁边。

---

## 项目结构

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

## 设计目标

MTKClient-RS 并不只是把一个 Python 工具重新实现为 Rust。

项目更关注以下几个方向：

**Native**

尽可能减少运行时依赖，让最终程序可以作为独立 Windows 工具分发。

**Data-driven**

将芯片、payload 等可变信息从核心代码中分离。

**Reusable**

通过 DA Session 复用降低重复初始化带来的开销。

**Resumable**

针对大容量分区传输提供可靠的状态保存和恢复能力。

**Extensible**

核心通信、分区、文件系统和数据层保持相对独立，方便后续接入 GUI 或其他平台。

---

## 安全与法律

本项目用于设备所有者本人进行学习、研究、维修、备份和开发测试。

涉及分区写入、擦除等操作时，请：

* 提前备份重要数据
* 确认目标设备和镜像文件
* 避免在传输过程中断开 USB
* 仅对你拥有或获得授权的设备进行操作
* 遵守当地法律法规

---

## License

Licensed under the **Apache License 2.0**.

See [`LICENSE`](LICENSE) for details.
