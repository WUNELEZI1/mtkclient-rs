# Penumbra 站点文档完整抓取

站点地址: https://penumbra.itssho.my/
Penumbra 是一个用 Rust 编写的 MTK 设备刷机工具核心（WIP），由 shomykohai 开发。
文档使用 Quartz 静态站点生成器构建。

---

## 站点导航结构（完整页面列表）

```
index (首页)
├── Penumbra/
│   ├── FAQ
│   └── Antumbra/
│       ├── CLI
│       └── TUI
├── Mediatek/
│   ├── Common/
│   │   ├── Seccfg
│   │   └── DA/
│   │       ├── Download-Agent
│   │       ├── XFlash-DA-Protocol
│   │       ├── XML-DA-Protocol
│   │       └── DA-Extensions
│   └── Exploits/
│       ├── Carbonara
│       ├── Heapbait
│       └── Kamakiri
```

---

## 1. XFlash DA Protocol（DA 协议文档）

**路径**: Mediatek/Common/DA/XFlash-DA-Protocol
**完整内容**:

### General Info
The XFlash DA Protocol (also known as V5 DA Protocol) is the communication protocol used by Download Agents in MediaTek devices to interact with the device during Download Mode.
It is an evolution of the older Legacy DA Protocol (which seem to be labeled as V3), which will also be the foundation for the later XML DA Protocol (V6).

### General Characteristics
The XFlash DA Protocol is found on most MediaTek devices released between 2016 and ~2022.
The communication happens over either UART (less common) or USB (most common).
The tool to use this protocol is SP Flash Tool V5.

### Communication layer
The XFlash DA Protocol (compared to Legacy), uses a more robust communication layer, where every packet is sent with an header.
The header is a 12 bytes structure with the following format:

- **Magic** (4 bytes): Always 0xFEEEEEEF
- **Data Type** (4 bytes): Indicates the type of data being sent. Either Protocol Flow (1) or Message (2). 99% of the time it's Protocol Flow (1).
- **Data Length** (4 bytes): Length of the data being sent, in bytes.

After the header, the actual data is sent.
When sending a packet to the device, the host will send an header followed by the data, the same way the device does. The device will read up to the Data Length specified in the header, and will ignore any extra data.

### Commands
The XFlash protocol divides commands in two categories: **Major Commands** and **Device Controls**.
A command is identified by a u32 ID.

#### Major Commands
Major Commands are the main commands used to interact with the device, and are mainly used to perform actions on the storage (flash, read, erase...).
Major commands are in the **0x0001XXXX** range.
A special major command is the **DeviceCtrl command (0x010009)**, which is used to use operate with the second set of commands.

#### Device Controls
These commands are usually "getters" and "setters" for device parameters, like Emmc info, setting checksum level, and generally to perform operations that are not directly related to storage operations.
Device Control are divided in ranges:

- **0x020000 - 0x02FFFF**: Setters (SetChecksumLevel, SetRemoteSecPolicy...)
- **0x040000 - 0x04FFFF**: Getters (GetEmmcInfo, GetUsbSpeed, GetChipId...)
- **0x080001 - 0x08FFFF**: Download controls? (StartDlInfo, EndDlInfo...)
- **0x0E0000 - 0x0EFFFF**: Storage Control? (CtrlStorageTest, DeviceCtrlReadRegister...)

To these, with DA Extensions, a new range was added:

- **0x0F0000 - 0x0FFFFF**: Extensions Controls (ReadRPMB, WriteRPMB, ReadRegister, WriteRegister, Sej...)

### Flow

#### Command flow
The general flow of communication using XFlash DA Protocol is as follows:

1. The host sends a command as a u32 LE value, preceded by the header.
2. The device responds with a status code (u32), 0x0 for success, other values for errors.
3. The device enters the command, and the host and device exchange data as needed.
4. After the command returns, the device sends a final status code (u32) indicating the result of the command.
5. The device is now ready for the next command.

For device controls, the flow is similar:

1. The host sends the DeviceCtrl command (0x010009) as a u32 LE value, preceded by the header.
2. The device responds with a status code (u32), 0x0 for success
3. The host sends the specific Device Control command ID
4. The device responds with a status code (u32), 0x0 for success
5. The host and device exchange data as needed.
6. After the command returns, the device sends a final status code (u32)
7. The device is now ready for the next command.

#### Download flow
During the download process, the host and device will exchange data in chunks.
To make sure the data is correctly received, the device and host will exchanges acknoledgements (0u32) after each chunk.

#### Progress report
During operation that might take some time (like erasing a partition), the device will enter a progress report mode, where it will periodically send progress updates to the host.
The device will send **0x40040004** followed by a progress percentage (u32, 0-100), indicating the operation is still ongoing.
When the operation is complete, the device will send **0x40040005** and the final status code (u32).

### Error codes
The XFlash protocol introduced a more robust error code system, with each code giving us information about what it means and the domain it belongs to.
The error codes are divided in 4 severity levels:

- **Success** (0x00000000)
- **Info** (0x40000000)
- **Warning** (0x80000000)
- **Error** (0xC0000000)

Then, follows the "domain" of this error code, which indicates which component the error relates to:

- Common (1)
- Security (2)
- Library (3)
- Device/HW (4)
- Host? (5)
- BROM (6)
- DA (7)
- Preloader (8)

Finally, the actual error code (0x01-...) is appended.
Example: `0xc0070004` => `0xC0000000` (Error) | `7 << 16` (domain) | `0x4` (code)

---

## 2. Seccfg 文档

**路径**: Mediatek/Common/Seccfg
**完整内容**:

The seccfg (Security Configuration) partition in Mediatek devices holds (as the name implies) security configurations.

### Seccfg V4 hex
In it, the following information are stored:

- Seccfg version
- Seccfg size
- Lock state
- Critical Lock state
- Sboot runtime

The seccfg V4 structure is as follows:

| Name             | Value                                        | Length |
|------------------|----------------------------------------------|--------|
| Magic Start      | 0x4D4D4D4D                                   | 4 bytes|
| Seccfg version   | 0x4 (seccfg v4)                              | 4 bytes|
| Lock state       | 0x1 (bootloader locked) / 0x3 (bootloader unlocked) | 4 bytes|
| Critical lock state | 0x1 (bootloader locked) / 0x0 (bootloader unlocked) | 4 bytes|
| Sboot runtime    | 0x0                                          | 4 bytes|
| Magic End        | 0x45454545 (Unless seccfg is malformed)       | 4 bytes|
| Encrypted Hash   | sha256 of the previous values packed together, then encrypted with SEJ, unique per device | 32 bytes|
| Padding          | 0x00                                         | Until 0x200 is reached |

---

## 3. Carbonara Exploit 文档

**路径**: Mediatek/Exploits/Carbonara
**完整内容**:

Carbonara is a memory corruption exploit targeting MediaTek devices. It allows loading unverified code during the Download Agent upload process for V5 and V6, basically letting you bypass the signature checks.

### How the Exploit Works
Carbonara abuses the **boot_to command** in DA1 to overwrite the stored reference hash. When you send boot_to followed by something like `a4de2200000000002000000000000000`, that hex string tricks boot_to into writing at the DA2 hash position.

Breaking down that hex string:
- `a4de22` translates to **0x22dea4** in little endian, which is the exact memory address where the legitimate DA2 hash is stored
- The `20000000` is **0x20** (32 bytes) in little endian, this is the hash length parameter telling DA1 to expect a 32-byte SHA-256 hash (would be 0x14 for SHA-1)
- The rest of the zeros are just a conseguence of packing the two parameters as two u64 (unsigned long long)

The attack works like this: send boot_to, send that crafted payload that points to the hash storage location and specifies the hash length, then send your malicious DA2's hash. This overwrites the reference hash with your own. When the actual malicious DA2 gets uploaded later through the normal process, its calculated hash matches the reference hash you just planted, so verification passes.

The device returns a DA_HASH_MISMATCH error (0xc0070004) after the first attempt, but the hash is already overwritten in memory at this point. You just resend boot_to and load your malicious DA2, which now passes verification because its hash matches what you planted.

### Technical Details
Communication with DA1 happens through the XFlash or XML protocol. The boot_to command expects:

1. Length and load address of the data that's going to be sent
2. The actual data

Carbonara tricks boot_to into writing at arbitrary addresses by crafting specific payloads. The DA2 hash location (like 0x22dea4 in the example) varies between DA versions but can be found by scanning DA1's binary for the legitimate DA2's hash.

The vulnerability exists because boot_to accepts any address, even those in DA1's own memory space (typically around 0x200000), while DA2 normally loads at 0x40000000.

In particular, the data being read from serial com is first stored into a buffer, then wrote into memory without first performing out of bounds checks or veryfing it beforehand.

This gap lets you overwrite DA1's stored hash before loading your modified DA2.

### The Fix
MediaTek fixed this by completely removing user control over the load address in boot_to. The patched versions now ignore whatever address you send and always force DA2 to load at 0x40000000.

Since the hash is stored in DA1's memory space (around 0x200000 range), you can no longer reach it with boot_to. Simple but effective, they just hardcoded the address instead of trying to validate user input.

**Before (vulnerable):**
(boot_to accepts any address from the host)

**After (patched):**
(boot_to ignores host-provided address, always uses 0x40000000)

Going more in depth of how this works, we need to understand what the channel->read function does in this context.
This function reads the data the host is sending from the serial port vcom, taking as arguments the buffer and length of the data being read.

The boot_to cmd expects the host to send some metadata about the data that it is going to be sent.
As we seen above, the first data sent is the boot address and size of the data that follows.

In the vulnerable DA, the address is trusted and used as the point where to put the data that is going to be read.

In addition, DA Extensions might be unavailable for Carbonara patched DAs even if a new exploit is found, because of the fix hardcoding the boot_to address.

### Checking Vulnerability
This can be checked by looking for specific patterns in the DA1 region. The mtkclient implementation checks for these byte patterns that indicate the patch is present.

Generally:
- Devices released before 2024 are vulnerable unless a DA update has been issued
- Devices released during or after 2024 aren't guaranteed to work, but there's been instances (e.g. Motorola G24, Honor 200 Lite)

> **Note**: Unless the device is unfused, a signed DA1 is required for this exploit to work, or you will be met with error 0xc0070004 (DA_HASH_MISMATCH) because of DAA being enabled!

**Authored by shomykohai, R0rt1z2.**

---

## 附加获取到的其他重要页面

### 4. Download Agent 概述

**路径**: Mediatek/Common/DA/Download-Agent
**完整内容**:

General Info: In Mediatek devices, a Download Agent (DA) is a special file containg code that is sent to the device through BROM or Preloader via serial communication.

The DA allows through specialized tools (like SP Flash Tool, mtkclient and penumbra) to perform a variety of specified actions on the device, most notably:

- Reading and writing flash
- Read and Write RPMB (Through DA Extensions or a specialized DA)
- Get info on the device (Chip ID, MEID, OTP ver...)
- Read and Write e-fuses
- Unlock or relock Seccfg (Through DA Extensions)

A Download Agent operates under three different protocols (ordered from oldest to newest):

1. Legacy (V3)
2. XFlash (V5)
3. XML (V6)

#### Download Agent stages
A DA file contains two stages: DA1 and DA2.

**DA1**: The first stage (DA1) is responsible to initializing the platform and environment for allowing then to boot the second stage.
DA1 (on secure chips) is bound to DAA (Download Agent Authorization) to allow to continue.
The Preloader will verify the signature of the first stage against the Public Key. If DAA doesn't pass, the first stage won't be loaded and an assertion will happen.
Exploits like Kamakiri allows to temporarily disable phone security protections (SLA, DAA and SBC) to be able to load an arbitrary patched DA1.
After the first stage is loaded, only some commands are available, most notably cmd_boot_to, which is used to load the second stage (DA2).
The boot_to cmd includes some protections, like hash and secure boot checks for allowing the incoming second stage to load.
Carbonara abuses a flawed implementation of this cmd to overwrite the DA2 hash contained in DA1, disrupting this way the whole root of trust and allowing to load an arbitrary payload.

**DA2**: The second stage (DA2) is what's responsible to use the full suite of commands, such as writing, reading and formatting partitions, writing efuses, read and write RPMB (before 2024).
DA2 can optionally implement DA SLA, a form of authentication similar to Preloader / Brom SLA, that (if present) needs to be completed before being able to execute any command.
Before some still unknown time in 2024, DA2 also implemented the boot_to cmd, which allowed DA Extensions to be arbitrary loaded with a patched second stage.

#### Download Agent Structure (V5)

| Data found          | Offset     | Description                                                          |
|---------------------|------------|----------------------------------------------------------------------|
| DA File Magic String| 0x0-0x12   | Always MTK_DOWNLOAD_AGENT                                           |
| DA File ID          | 0x20-0x60  | In XFlash: MTK_AllInOne_DA_v3 / In XML: MTK_DA_v6                   |
| DA Version          | 0x60-0x64  | Seem to always stay 4 in all DA files                                |
| DA Magic            | 0x64-0x68  | Always 99886622                                                      |
| Number of SoC       | 0x68-0x6C  | How many DA entries are in one file                                   |
| DA Entries          | 0x6C-0x??  | Each DA entries contains metadata on the DA and their regions.       |

Each DA Entry (0xDC on XFlash/XML, 0xD8 on Legacy):

| Data found         | Offset     | Description                                    |
|--------------------|------------|------------------------------------------------|
| Magic              | 0x0-0x02   | Seems to always be DADA                        |
| HW Code (chipset)  | 0x02-0x04  | Which chipset this DA entry works on           |
| HW Sub code        | 0x04-0x06  | Chipset subcode                                 |
| HW Version         | 0x06-0x08  | Another Identifier for the chipset revision     |
| Entry region index | 0x10-0x12  | Seem to always be 0                             |
| Entry region count | 0x12-0x14  | How many regions this DA Entry has              |
| Region table       | 0x14-0xDC  | Metadata on each region (0x20 bytes per region)  |

Each Region (0x20 bytes):

| Data found        | Offset     | Description                                      |
|-------------------|------------|--------------------------------------------------|
| Offset            | 0x0-0x04   | At which offset in the DA file this region starts |
| Length            | 0x04-0x08  | Length of this region (Signature included)         |
| Address           | 0x08-0x0C  | Address where this region will be loaded           |
| Region length     | 0x0C-0x10  | Same as length, minus signature length            |
| Signature length  | 0x10-0x14  | How many bytes the signature is long              |

#### Download Agent Security
- **DA SLA**: After DA2 gets uploaded and executed, auth will be required to continue. The auth is an RSA key, usually found in the SLA_Challenge.dll file.
- **DAA (Download Agent Authorization)**: Verifies DA1 signature against the public key stored in the device efuses. Not DA specific, but needed for booting the DA.

### 5. DA Extensions

**路径**: Mediatek/Common/DA/DA-Extensions

What are DA Extensions?
DA Extensions are payloads that work like an addon for stock Download Agents, allowing to extend the features of it.
Many tools use the extensions developed by bkerler for mtkclient, some others like Penumbra or Chimera have their owns.
DA Extensions are available for XFlash and XML Download Agents.

How do they work?
To load DA Extensions, you first need to be able to boot patched download agents (or at least, a custom DA2).
This is to ensure hash check is disabled.
The DA Extensions are loaded at **0x68000000** (which usually is located in the DA2 far heap space), to ensure the original DA2 is not being overwritten.
The load address is not particularly important as long as it doesn't interfere with normal execution.
In a 2025 mtkclient update, a PR was merged to allow loading extensions at **0x4FFF0000** for low memory devices using XFlash protocol.
Before being sent, the DA extension binary is patched to hook into the original DA2 handlers.

Features:
- Restored memory read and write command (Registers)
- RPMB read and write
- SEJ AES (Encryption & Decryption with HW based crypto)
- Key derivation (RPMB & FDE)

### 6. Heapbait Exploit

**路径**: Mediatek/Exploits/Heapbait

heapb8 is a heap overflow exploit targeting MediaTek's second stage Download Agent (DA2). It allows loading unsigned code and bypassing security checks on devices that use V6 and have been patched against Carbonara.
The exploit was originally discovered by the Chimera Tool team and later reverse engineered and implemented into penumbra by R0rt1z2 and shomy.

How the Exploit Works:
heapb8 abuses a buffer overflow in DA2's USB file download handler (fp_read_host_file) to corrupt heap metadata and hijack the DA's DPC mechanism in order to redirect execution to arbitrary shellcode.
This is accomplished through CMD:SECURITY-SET-ALLINONE-SIGNATURE.

**Stage 1**: First command sends a shellcode payload with a large NOP sled (~80MB) to avoid having to predict the exact heap address.

**Stage 2**: Second command with a ~5KB filename causes mxmlLoadString to allocate a buffer landing immediately after the AIO2 data buffer in the heap:
[AIO1 shellcode buffer] ... [AIO2 data buffer (0x1400)] [XML filename buffer (0x13A0)]

**Stage 3**: fp_read_host_file vulnerability - the read loop uses packet_size (0x20000) per chunk instead of the remaining capacity, causing 0x10 bytes overflow past the AIO2 buffer, corrupting the XML filename buffer's alloc_struct_begin header.

**Stage 4**: On ARM64, by overwriting the header with ptr = dpc_addr - 0x10 and size = shellcode_addr, the free() call writes shellcode_addr directly into dpc->cb.

**Stage 5**: On the next command loop iteration, the shellcode executes via the DPC mechanism.

**Authored by R0rt1z2.**

---

## 重点分析

### XFlash DA Protocol 中可能遗漏的命令或流程

1. **Device Control 范围已完整列出**:
   - 0x020000-0x02FFFF: Setters
   - 0x040000-0x04FFFF: Getters
   - 0x080001-0x08FFFF: Download controls
   - 0x0E0000-0x0EFFFF: Storage Control
   - 0x0F0000-0x0FFFFF: Extensions Controls (DA Extensions 新增)

2. **可能遗漏的注意点**:
   - 文档提到 "Major commands are in the 0x0001XXXX range" 但没有列出所有具体 Major Command ID 的完整列表（如读分区、写分区、格式化等具体命令号）。只有 DeviceCtrl (0x010009) 被明确给出。
   - 进度报告的两个特殊标识符: 0x40040004 (进行中) 和 0x40040005 (完成) 值得注意。
   - Data Type 字段虽然 99% 是 Protocol Flow (1)，但 Message (2) 类型的用途文档未说明。

### Seccfg 中偏移 0x10 字段的官方定义

文档中的 Seccfg V4 结构**没有显式标注偏移地址**，但根据字段顺序和长度推算:

| 偏移   | 字段             | 值/说明                                                | 长度     |
|--------|------------------|--------------------------------------------------------|----------|
| 0x00   | Magic Start      | 0x4D4D4D4D                                             | 4 bytes  |
| 0x04   | Seccfg version   | 0x4                                                    | 4 bytes  |
| 0x08   | Lock state       | 0x1 (locked) / 0x3 (unlocked)                         | 4 bytes  |
| 0x0C   | Critical lock    | 0x1 (locked) / 0x0 (unlocked)                         | 4 bytes  |
| **0x10** | **Sboot runtime** | **0x0**                                              | **4 bytes** |
| 0x14   | Magic End        | 0x45454545                                             | 4 bytes  |
| 0x18   | Encrypted Hash   | sha256(previous values) encrypted with SEJ             | 32 bytes |
| 0x38   | Padding          | 0x00                                                   | 到 0x200 |

**偏移 0x10 的官方定义是 "Sboot runtime"，固定值为 0x0，长度 4 字节。**

### Carbonara 中的额外漏洞利用技巧

1. **SHA-1 支持**: 文档提到 hash length 参数对于 SHA-256 是 0x20 (32 字节)，对于 SHA-1 是 0x14 (20 字节)。这意味着如果设备使用 SHA-1 哈希验证，Carbonara 同样适用，只需调整长度参数。

2. **hash 位置因 DA 版本而异**: "The DA2 hash location (like 0x22dea4 in the example) varies between DA versions but can be found by scanning DA1's binary for the legitimate DA2's hash." -- 可以通过扫描 DA1 二进制文件找到合法 DA2 的哈希值，从而确定目标地址。

3. **错误码可被忽略**: 设备返回 DA_HASH_MISMATCH (0xc0070004) 后，哈希已被覆写。只需重新发送 boot_to 并加载恶意 DA2 即可。

4. **补丁的副作用**: Carbonara 的修复（硬编码地址为 0x40000000）不仅阻止了该漏洞，还可能导致 DA Extensions 在已修补设备上不可用，因为 boot_to 地址被硬编码了。

5. **2024年后设备仍有漏洞实例**: 如 Motorola G24 和 Honor 200 Lite 仍然存在漏洞，说明并非所有新设备都打了补丁。