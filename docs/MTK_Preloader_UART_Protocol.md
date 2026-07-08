# MTK Preloader UART Protocol Reference

## 概述

MediaTek Preloader 是设备启动链中的第二阶段引导程序，位于 BROM（Boot ROM）之后、LK（Little Kernel）之前。它通过 UART/USB 提供一系列低级服务：分区读写、内存访问、固件上传、重启模式切换等。

本文档基于以下来源整理：
- mtkclient 开源项目（GPL-3.0）Python 实现
- MTK FlashTool 协议逆向分析
- 实际设备（MT6768/MT6769）串口抓包验证

---

## 连接层

### 串口参数

| 参数 | 值 |
|------|-----|
| 波特率 | 115200 |
| 数据位 | 8 |
| 停止位 | 1 |
| 校验 | None |
| 流控 | None |

### 设备识别

Preloader 模式设备在 Windows 上表现为 COM 口，通常标识为：
- **VID**: 0x0E8D
- **PID**: 0x2000（Preloader VCOM）或 0x0003（DA 模式）

---

## 握手协议

### BROM 握手（逐字节验证）

BROM 模式下使用逐字节握手，每个字节发送后等待取反响应：

```
Host → Device: 0xA0
Device → Host: ~0xA0 = 0x5F
Host → Device: 0x0A
Device → Host: ~0x0A = 0xF5
Host → Device: 0x50
Device → Host: ~0x50 = 0xAF
Host → Device: 0x05
Device → Host: ~0x05 = 0xFA
```

### Preloader 握手

Preloader 模式下握手方式取决于具体实现。部分设备在连接后持续发送 `"READY"`（5 字节 ASCII）等待命令。

---

## 标准命令字节

Preloader 使用单字节命令，命令字节发送后设备回复其按位取反（~cmd）作为确认。

### 命令表

| 命令字节 | 名称 | 说明 |
|----------|------|------|
| 0x70 | SEND_PARTITION_DATA | 发送分区数据 |
| 0x71 | JUMP_TO_PARTITION | 跳转到分区 |
| 0x72 | CHECK_USB_CMD | 检查 USB 命令 |
| 0x80 | STAY_STILL | 保持当前状态 |
| 0x88 | CMD_88 | 未知命令 |
| 0xA2 | CMD_READ16_A2 | 读取 16 位数据 |
| 0xB0 | I2C_INIT | 初始化 I2C |
| 0xB1 | I2C_DEINIT / JUMP_MAUI | 释放 I2C / 跳转到 MAUI |
| 0xB2 | I2C_WRITE8 | I2C 写 8 位 |
| 0xB3 | I2C_READ8 | I2C 读 8 位 |
| 0xB4 | I2C_SET_SPEED | 设置 I2C 速度 |
| 0xB6 | I2C_INIT_EX | 扩展 I2C 初始化 |
| 0xB8 | I2C_WRITE8_EX / READY | 扩展 I2C 写 / READY 状态 |
| 0xB9 | I2C_READ8_EX | 扩展 I2C 读 |
| 0xBA | I2C_SET_SPEED_EX | 扩展 I2C 速度设置 |
| 0xBF | GET_MAUI_FW_VER | 获取 MAUI 固件版本 |
| 0xC1 | OLD_SLA_SEND_AUTH | 旧版 SLA 发送认证 |
| 0xC2 | OLD_SLA_GET_RN | 旧版 SLA 获取随机数 |
| 0xC3 | OLD_SLA_VERIFY_RN | 旧版 SLA 验证随机数 |
| 0xC4 | PWR_INIT | 电源初始化 |
| 0xC5 | PWR_DEINIT | 电源释放 |
| 0xC6 | PWR_READ16 | 电源读取 16 位 |
| 0xC7 | PWR_WRITE16 | 电源写入 16 位 |
| 0xC8 | CMD_C8 / EXT_CMD_GATE | 扩展命令入口 |
| 0xD0 | READ16 | 读取 16 位 |
| 0xD1 | READ32 | 读取 32 位 |
| 0xD2 | WRITE16 | 写入 16 位 |
| 0xD3 | WRITE16_NO_ECHO | 写入 16 位（无回显） |
| 0xD4 | WRITE32 | 写入 32 位 |
| 0xD5 | JUMP_DA | 跳转到下载代理（DA） |
| 0xD6 | JUMP_BL | 跳转到 Bootloader（LK） |
| 0xD7 | SEND_DA | 发送下载代理 |
| 0xD8 | GET_TARGET_CONFIG | 获取目标配置（安全状态） |
| 0xD9 | SEND_ENV_PREPARE | 发送环境准备 |
| 0xDA | BROM_REG_ACCESS | BROM 寄存器访问 |
| 0xDB | UART1_LOG_EN | UART1 日志使能 |
| 0xDC | UART1_SET_BAUDRATE | 设置 UART1 波特率 |
| 0xDD | BROM_DEBUGLOG | BROM 调试日志 |
| 0xDE | JUMP_DA64 | 跳转到 64 位 DA |
| 0xDF | GET_BROM_LOG_NEW | 获取新 BROM 日志 |
| 0xE0 | SEND_CERT | 发送证书 |
| 0xE1 | GET_ME_ID | 获取 ME ID |
| 0xE2 | SEND_AUTH | 发送认证 |
| 0xE3 | SLA | 安全启动认证（SLA） |
| 0xE4 | CMD_E4 | 未知命令 |
| 0xE5 | CMD_E5 | 未知命令 |
| 0xE6 | CMD_E6 | 未知命令 |
| 0xE7 | GET_SOC_ID | 获取 SoC ID |
| 0xE8 | CMD_E8 | 未知命令 |
| 0xF0 | ZEROIZATION | 零化 |
| 0xFB | GET_PL_CAP | 获取 Preloader 能力 |
| 0xFA | CMD_FA | 未知命令 |
| 0xFC | GET_HW_SW_VER | 获取硬件/软件版本 |
| 0xFD | GET_HW_CODE | 获取硬件代码 |
| 0xFE | GET_BL_VER | 获取 Bootloader 版本 |
| 0xFF | GET_VERSION | 获取版本 |

### 0xC8 扩展命令

`0xC8` 是扩展命令入口。发送 `0xC8` 后，设备回显 `0xC8`，然后发送实际子命令字节：

```
Host → Device: 0xC8
Device → Host: 0xC8 (回显)
Host → Device: 0xB1 (子命令)
Device → Host: 0xB1 (回显)
```

---

## Pattern 协议（BootMode Switch）

Pattern 协议是 Preloader 的一种扩展机制，用于在不加载 DA 的情况下切换启动模式（fastboot、factory、meta 等）。

### 协议流程

```
1. 标准 Preloader 握手
2. 发送 8 字节 Pattern（模式标识符的反转字符串）
3. 读取设备回复的 "READY"（5 字节 ASCII）
4. 发送 BootMode Switch Request 参数结构体
5. 设备重启到目标模式
```

### Pattern 映射表

Pattern 是 8 字节的 ASCII 字符串，为目标模式名称的反转（或特定编码）。

| 目标模式 | Pattern（8 字节） | 原始字符串 |
|----------|-------------------|-----------|
| FASTBOOT (bootloader) | `DMHCTIWS` | "FASTBOOT" reversed |
| FACTORY (recovery) | `MYROTCAF` | "FACTORYM" reversed |
| META | `ATEM    ` | "META" + padding |
| ATE / META | `ATEMATEM` | "ATEMATEM" |
| ATE EvDA | `ATEMEVDA` | "ATEMEVDA" |
| ATE EvDx | `ATEMEVDX` | "ATEMEVDX" |
| Advanced META | `ADVEMETA` | "ADVEMETA" |
| ATE Factory | `FACTFACT` | "FACTFACT" |
| DualTalk Switch | `SWITCHMD` | "SWITCHMD" |

### BootMode Switch Request 参数

参数结构体格式（小端序，从 mtk.exe 逆向提取）：

```
04 00 00 00  01 00 00 00  01 00 00 00
```

共 12 字节，含义推测：
- 第 1 字段（4 字节）：命令类型 = 0x04
- 第 2 字段（4 字节）：子类型 = 0x01
- 第 3 字段（4 字节）：标志 = 0x01

### 实现示例（Rust）

```rust
fn send_boot_pattern(device: &mut dyn BromTransport, pattern: &[u8; 8]) -> Result<(), String> {
    // 1. 发送 8 字节 Pattern
    device.write(pattern)?;

    // 2. 读取 READY 确认
    let mut ready = [0u8; 5];
    device.read_exact(&mut ready)?;
    assert_eq!(&ready, b"READY");

    // 3. 发送 Switch Request
    let switch_req = [0x04, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
    device.write(&switch_req)?;

    Ok(())
}
```

---

## DA 协议（XFlash）

DA（Download Agent）是 MTK 的固件上传/下载协议，运行在 Preloader 之上。

### DA 加载流程

```
1. Preloader 握手
2. 发送 SEND_DA (0xD7) + DA1 数据
3. JUMP_DA (0xD5) 跳转到 DA1
4. DA1 初始化后，上传 DA2
5. 握手完成，进入 XFlash 模式
```

### DA 扩展（DA Patch）

DA 扩展用于绕过安全检查（SLA/DAA）：

```
1. 读取 DA 哈希校验策略
2. 计算 DA 哈希并绑定
3. Patch hash_check 函数（跳过校验）
4. Patch get_vfy_policy 函数（返回允许策略）
5. 发送扩展数据到 DA
```

### XFlash 命令

| 命令 | 值 | 说明 |
|------|-----|------|
| CMD_WRITE_DATA | 0x040001 | 写入数据到闪存 |
| CMD_READ_DATA | 0x040002 | 从闪存读取数据 |
| CMD_FORMAT | 0x040005 | 格式化分区 |
| CMD_WRITE_PT | 0x040006 | 写入分区表 |
| CMD_READ_PT | 0x040007 | 读取分区表 |
| CMD_GET_EMMC_INFO | 0x040008 | 获取 eMMC 信息 |
| CMD_GET_HW_INFO | 0x040009 | 获取硬件信息 |

---

## 安全状态

### GET_TARGET_CONFIG (0xD8) 响应

设备返回 4 字节安全状态位掩码：

| 位 | 名称 | 说明 |
|----|------|------|
| 0 | SECURE_BOOT | 安全启动已启用 |
| 1 | SERIAL_LINK_AUTH | 串口链路认证（SLA）已启用 |
| 2 | DOWNLOAD_AGENT_AUTH | 下载代理认证（DAA）已启用 |

### 绕过方式

| 保护类型 | 绕过方法 |
|----------|---------|
| SLA (Serial Link Authorization) | Kamakiri2 漏洞利用（发送无效地址触发越界写） |
| DAA (Download Agent Authorization) | 同上 |
| SBC (Secure Boot Check) | DA Patch 修改 hash_check 函数 |

---

## 分区操作

### GPT 分区表

设备使用 GPT（GUID Partition Table）管理分区。Preloader 支持读取 GPT 头部和条目。

### 动态分区（Android Super）

Android 10+ 设备使用动态分区，物理 `super` 分区内部包含多个 logical partition：

| Logical Partition | 说明 |
|-------------------|------|
| system | Android 系统分区 |
| vendor | 厂商驱动分区 |
| product | 产品定制分区 |
| system_ext | 系统扩展分区 |

动态分区元数据位于 `super` 分区头部，通过 `LP_METADATA_GEOMETRY`（magic = 0x414C4147）定位。

---

## 参考实现

### mtkclient（Python，GPL-3.0）
- https://github.com/bkerler/mtkclient
- 完整的 BROM/Preloader/DA 协议实现
- 支持 bypass、分区读写、seccfg 解锁等

### ZybFlashTool（Rust，GPL-3.0）
- 本工具实现
- 使用 nusb（纯 Rust USB 库）+ WinUSB 直连
- 支持流式 I/O、动态分区解析

---

## 免责声明

本文档中的协议信息来源于以下途径：
1. mtkclient 开源项目（GPL-3.0 许可证）的公开源代码
2. 从公开可获取的二进制文件中提取的 ASCII 字符串（strings 分析）
3. 实际设备的串口通信实验验证

本文档仅用于技术研究、设备维修和教育目的。使用本文档中的信息对设备进行修改可能导致：
- 设备保修失效
- 数据丢失
- 设备变砖（无法启动）

作者不对因使用本文档信息造成的任何损失负责。请在使用前备份重要数据。

MediaTek 及其相关商标归联发科技股份有限公司所有。本文档与 MediaTek 官方无关。
