# MTKClient-RS 项目文件索引与代码文档

## 项目概述

MTKClient-RS 是 MediaTek 设备刷写工具的 Rust 实现，用于读写 MTK 设备的分区、解锁 bootloader 等操作。基于 libusb1-sys 实现低层 USB 通信，对齐 Python mtkclient 的 BROM/Preloader/XFlash 协议。

***

## 项目结构

```
d:\test\ZybClient\
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── commands.rs
│   ├── config.rs
│   ├── usb.rs
│   ├── preloader.rs
│   ├── da_xflash.rs
│   ├── da_extension.rs
│   ├── da_partition.rs
│   ├── kamakiri2.rs
│   ├── usb_diag.rs
│   ├── driver.rs
│   └── paths.rs
├── mtkclient-2.0.1/
├── mtkclient-2.1.4.1/
├── payloads/
├── usb_driver/
└── target/
```

***

## 文件索引

### 1. Cargo.toml

**路径**: `d:\test\ZybClient\Cargo.toml`

项目配置文件。依赖 libusb1-sys（原生 C 库绑定）、clap（CLI 解析）、serialport（串口通信）、sha2/aes/cbc（加密操作）。

```toml
[package]
name = "mtkclient-rs"
version = "0.1.0"
edition = "2024"

[dependencies]
libusb1-sys = "0.7"
log = "0.4"
env_logger = "0.11"
colored = "2"
clap = { version = "4", features = ["derive"] }
serialport = "4.7"
sha2 = "0.10"
aes = "0.8"
cbc = "0.1"
```

***

### 2. src/main.rs

**路径**: `d:\test\ZybClient\src\main.rs`

程序主入口。设置 UTF-8 控制台编码，解析 CLI 参数，初始化日志，执行命令调度。

关键流程：
- `smart_init`：等待设备连接，带 USB 诊断（UsbDiagState），区分 NoDevice/WrongDriver/Preloader/Brom/EndpointError/HandshakeFailed
- BROM 模式下自动 dump preloader（`dump-preloader` 命令或无指定 preloader 文件时）
- 支持 `--batch` 批量执行多个命令，保持 DA 会话
- 支持 `--no-device` 离线模式处理 seccfg 文件
- 支持 `dump-preloader`、`dumpbrom`、`printgpt`、`r`/`read`、`w`/`write`、`e`/`erase`、`vbmeta`、`unlock`、`lock`、`reset`、`enable-adb-on-da` 等命令
- 诊断命令：`diagnose`、`list-usb`、`check-driver`
- 驱动安装：`install-drivers`

```rust
fn smart_init(context: &UsbContext) -> Result<(usb::UsbDevice, DeviceMode), String>
fn parse_sub_commands(first_cmd: &str, args: &[String]) -> Vec<(String, Vec<String>)>
```

***

### 3. src/cli.rs

**路径**: `d:\test\ZybClient\src\cli.rs`

命令行参数定义，使用 clap derive 宏。

参数包括：`--da2`、`--preloader`、`--loader`、`--parttype`、`--offset`、`--length`、`--sector`、`--sectors`、`--verify`、`--debug-mode`、`--check-driver`、`--force`、`--no-device`、`command`、`args`。

***

### 4. src/config.rs

**路径**: `d:\test\ZybClient\src\config.rs`

配置模块，统一管理设备类型、芯片参数、安全配置。

**DeviceType**：根据 VID/PID 判断 BROM/Preloader/PreloaderVariant/Unknown，影响握手策略。

**SUPPORTED_DEVICES**：MTK BROM (0x0003/F200/D1E9/D1E2/D1EEC/D1DD)、Preloader (0x2000)、Preloader Variant (0x2001)。

**ChipConfig**：芯片完整配置，包含：
- hw_code、name、description、loader
- watchdog、uart 地址
- brom/da/pl payload 地址
- gcpu/sej/dxcc/cqdma/ap_dma 基址
- send_ptr/ctrl_buffer/cmd_handler
- brom_register_access 地址
- meid/socid/prov/misc_lock/efuse 地址
- blacklist/blacklist_count
- ptr_da_bra / ptr_send_addr（Kamakiri2 特殊地址）

支持芯片：MT6768/MT6769 (0x0707)、MT6771 (0x0788)。

**TargetConfig**：设备安全配置，解析 4 字节 big-endian 位域：
- sbc (0x01)、sla (0x02)、daa (0x04)、swjtag (0x06)
- epp (0x08)、cert (0x10)、memread (0x20)、memwrite (0x40)、cmd_c8 (0x80)
- `needs_bypass()`：判断是否需要绕过安全保护

***

### 5. src/usb.rs

**路径**: `d:\test\ZybClient\src\usb.rs`

USB 通信核心模块，基于 libusb1-sys。

**UsbContext**：libusb 上下文管理，init/exit。

**UsbDevice**：
- `new()`：查找 MTK 设备，detach kernel driver，claim interface 0+1，扫描端点
- `write()`：bulk OUT 传输，支持 ZLP（空数据），含 TX trace 日志
- `read()`：bulk IN 传输，带 deadline 重试循环，支持 `QUIET_USB_READ` 静默模式
- `read_exact()`：单次 bulk transfer 精确读取，不重试，用于 payload 响应
- `ctrl_transfer_in/out()`：控制传输
- `clear_halt_in/out()`：清除端点 halt/stall 状态
- `do_handshake()`：4 字节握手协议（0xA0 0x0A 0x50 0x05），BROM 模式直接握手，Preloader 先发送 0xA0，最多 10 次尝试
- `reopen()`：重新打开 USB 设备句柄
- `set_timeout/get_timeout()`：超时管理

**USB Trace**：`usb_trace()` 函数记录 TX/RX 数据到 `usb_debug.log`，格式 `[HH:MM:SS.mmm] [TX/RX] [file:line] hex_data`。可通过 `set_usb_log_enabled()` 开关。

***

### 6. src/preloader.rs

**路径**: `d:\test\ZybClient\src\preloader.rs`

BROM/Preloader 协议处理，核心通信层。

**关键方法**（完全对齐 Python mtkclient）：

- `init()`：基础握手确认（`do_handshake`）
- `echo_1byte(cmd)`：1 字节 echo 协议，对齐 Python `Port.echo()`，write + read_exact 1 字节比较
- `sendcmd(cmd)`：发送 1 字节命令（调用 `echo_1byte`）
- `get_hw_code()`：echo(0xFD) → 读 4 字节 big-endian → 取高 16 位为 HW code
- `get_target_config()`：先获取 HW code 设置 chip，echo(0xD8) → 读 6 字节 → 解析 TargetConfig
- `send_da()`：echo(0xD7) → write(addr/size/sig_len 大端) → rword() 读状态 → 64 字节分块上传数据 → ZLP → 读 checksum + status2
- `jump_da()`：echo(0xD5) → write(addr 大端) → rdword() 验证 → rword() 读状态
- `jump_bl()`：echo(0xD6) → rword() → 若 <=0xFF 再 rword()
- `brom_register_access()`：echo(0xDA) → write(mode/addr/len 大端) → 读状态 → 读/写数据 → 读状态2
- `read32_brom()`：封装 `brom_register_access` 读 BROM 内存
- `rbyte(n)`：读 n 字节
- `rword()`：读 2 字节 big-endian u16
- `rdword()`：读 4 字节 big-endian u32

注意：所有参数发送使用 `device.write()` 而非 echo，对齐已验证的工作版本。

***

### 7. src/kamakiri2.rs

**路径**: `d:\test\ZybClient\src\kamakiri2.rs`

Kamakiri2 漏洞利用实现，作为 `Preloader` 的 impl 扩展模块（`#[path = "kamakiri2.rs"] mod kamakiri2` 在 preloader.rs 中引入）。

**核心功能**：
- `kamakiri2_step()`：通过 ctrl_transfer_out(0x21, 0x20) + ctrl_transfer_in 执行 exploit 步骤
- `da_setup()`：brom_register_access + read32_brom + 3 步 kamakiri2
- `da_read()`：通过 kamakiri2 漏洞读 BROM 内存（addr<0x40 和 addr>=0x40 不同路径）
- `da_write()`：通过 kamakiri2 漏洞写 BROM 内存
- `inject_payload()`：完整 payload 注入流程：获取 linecode → 读 ptr_send → da_write payload → 等待 ack (0xA1A2A3A4)
- `run_kamakiri2()`：完整 exploit 流程，返回 preloader 数据和文件名
- `run_payload()` / `run_payload_from_data()`：通用 payload 注入
- `dump_preloader_from_ram()`：从 RAM 流式 dump preloader（read32_brom 循环）
- `bypass_security()`：注入 patcher payload 绕过 SBC/SLA/DAA
- `run_dump_brom_payload()`：通过 dump payload 提取 BROM
- `dump_preloader_payload()`：通过 preloader dump payload 流式提取 preloader，支持 quiet 模式，自动从 MTK_BLOADER_INFO 提取文件名
- `dump_brom()`：等待并接收 BROM 数据（分块读，显示进度）

**ptr_send_addr/ptr_da_bra**：优先使用 ChipConfig 中的 `ptr_send_addr`/`ptr_da_bra`，否则回退到 `send_ptr.1`/`brom_register_access.1`。

***

### 8. src/da_xflash.rs

**路径**: `d:\test\ZybClient\src\da_xflash.rs`

XFlash 协议实现，DA 加载和分区操作核心。

**XFlash 命令常量**：
- CMD_MAGIC (0xFEEEEEEF)、CMD_SYNC_SIGNAL (0x434E5953)
- CMD_SETUP_ENVIRONMENT (0x010100)、CMD_SETUP_HW_INIT_PARAMS (0x010101)
- CMD_WRITE_DATA (0x010004)、CMD_FORMAT (0x010003)、CMD_READ_DATA (0x010005)
- SET_META_BOOT_MODE (0x020006)

**DAXFlash 结构体**：
- `new()`：创建实例
- `rword()/rdword()`：读 2/4 字节 little-endian（XFlash 协议用）
- `load_preloader_emi()`：从 preloader 文件提取 EMI 数据
- `extract_emi()`：查找 MTK_BLOADER_INFO_v 标记，解析 EMI 版本和数据
- `xflash_sync()`：发送 SYNC_SIGNAL，不读响应
- `setup_env()`：设置环境（da_log_level、log_channel、system_os、ufs_provision）
- `setup_hw_init()`：初始化硬件参数
- `upload_data()`：分块上传数据（64 字节块，每 0x2000 字节发 ZLP）
- `send_emi()`：发送 EMI 数据初始化 DRAM
- `boot_to()`：上传并跳转到指定地址（CMD_BOOT_TO → send_data → 等）
- `upload_da1()`：上传 Stage1 DA，patch DA1，jump_da，XFlash 同步
- `upload_da2()`：上传 Stage2 DA，patch DA2，boot_to
- `upload_da()`：完整 DA 加载流程（DA1 → expire_date → reset_key → checksum_level → connection_agent → EMI → DA2 → SLA → reinit → extensions）
- `send_devctrl()`：发送设备控制命令（DEVICE_CTRL → 具体命令 → 参数/响应）
- `status()`：读 XFlash 状态（12 字节头 + 数据）
- `xread_data()`：读 XFlash 数据包
- `reinit()`：获取 RAM/芯片/EMMC/DA 版本/Random ID 信息
- `readflash_data()`：读 flash 数据（READ_DATA → send_param → xread 循环 → ack）
- `ack()`：发送 ack 包（3 字节 header + 4 字节 0）
- `get_emmc_info()`：获取 EMMC Boot1/Boot2 大小
- `generate_da_extensions()`：生成 DA extensions 二进制（调用 da_extension.rs）
- `patch_vbmeta()`：修补 vbmeta（占位实现）
- `close_device()`：关闭设备，可选发送 jump_bl 重启

**SEJ 加密**（seccfg 处理）：
- `sej_sec_cfg_sw_decrypt/encrypt`：AES-256-CBC 软件模式
- `sej_sec_cfg_hw_v3_encrypt/decrypt`：AES-128-CBC 硬件模式 V3/V4
- `sej_sec_cfg_hw_encrypt/decrypt`：AES-128-CBC 硬件模式 V2
- `generate_custom_seed_iv()`：生成 V3/V4 硬件模式 IV

**SecCfgV4**：解析和修改 V4 seccfg（magic=0x4D4D4D4D，SHA256 哈希 + AES 加密）
**SecCfgV3**：解析和修改 V3 seccfg（info_header + 加密数据段 + endflag）

**离线模式**：
- `detect_seccfg_version()`：自动检测 V3/V4
- `seccfg_unlock_offline()`：离线解锁 seccfg 文件
- `seccfg_lock_offline()`：离线锁定 seccfg 文件

**DA 文件解析**：
- `parse_da_header()`：解析 AllInOne DA 文件格式，匹配 HW Code
- `parse_da_regions()`：解析 DA region 信息

**GPT 解析**：
- `parse_gpt_from_data()`：从数据解析 GPT 分区表（自动检测偏移）
- `generate_scatter_from_gpt()`：生成 SP Flash Tool scatter 文件
- `read_gpt_from_file()`：从文件分析 GPT

***

### 9. src/da_extension.rs

**路径**: `d:\test\ZybClient\src\da_extension.rs`

DA 修补和扩展生成模块，作为 `DAXFlash` 的 impl 扩展。

**DA 修补**：
- `find_binary()`：搜索二进制模式（支持 `.` 0x2E 作为单字节通配符）
- `apply_patches()`：应用修补补丁
- `patch_da1()`：修补 DA1（oppo security、mt6739 c30、ram blacklist、seclib_sec_usbdl_enabled、hash_check3、version check、hash_check、hash_check2）
- `patch_da2()`：修补 DA2（通用 + huawei/oppo security、hash binding/check、security check、anti-rollback、SBC、register read/write、write not allowed）

**DA Extensions 生成**：
- `generate_da_extensions()`：从 DA2 中查找关键函数地址（register_devctrl、mmc_get_card、mmc_set_part_config、mmc_rpmb_send_command、g_ufs_hba、ufshcd_get_free_tag、ufshcd_queuecommand），填充到 da_x.bin 模板的占位符（\x11\x11\x11\x11 等）
- `find_binary_wildcard()`：带通配符的字节搜索

**扩展功能**：
- `custom_readmem()`：通过 DA2 读物理内存（CUSTOM_READMEM 0x0F0001，最大 0x10000 字节/块）
- `set_meta()`：设置 meta boot 模式（usb/off）
- `enable_adb_and_reboot()`：在 DA 模式下开启 ADB 并重启

***

### 10. src/da_partition.rs

**路径**: `d:\test\ZybClient\src\da_partition.rs`

分区操作模块，作为 `DAXFlash` 的 impl 扩展。

**GPT 处理**：
- `GptInfo`：GPT 分区表信息结构（base、num_part_entries、part_entry_size、part_entry_start_lba、first_usable_lba）
- `parse()`：从 GPT 数据解析
- `for_each_partition()`：遍历所有分区，调用回调

**分区操作**：
- `read_gpt()`：读取 GPT 分区表（USB 版本），保存原始数据到 `gpt_full.bin`
- `find_partition_addr()`：查找分区的物理地址和大小
- `read_partition()`：读取分区数据到文件（自动读 GPT → 找地址 → readflash_data → 写入文件）
- `write_partition()`：写入文件到分区（cmd_write_data → 循环分包 [0x0(4B)][checksum(4B)][data] → status → CC_OPTIONAL_DOWNLOAD_ACT）
- `write_partition_with_verify()`：写入并校验
- `erase_partition()`：擦除分区（FORMAT 命令 → send_param → 等待 STATUS_COMPLETE 0x40040005）
- `get_packet_length()`：获取写包长度
- `cmd_write_data()`：发送写命令

**Bootloader 操作**：
- `unlock_bootloader()`：解锁 Bootloader（读 seccfg → 解析 V4/V3 → 修改 lock_state → 重新签名 → 写回）
- `lock_bootloader()`：锁定 Bootloader

**SecCfgV4/SecCfgV3**：简化的 seccfg 结构体（与 da_xflash.rs 中的完整版不同，用于在线模式）

***

### 11. src/commands.rs

**路径**: `d:\test\ZybClient\src\commands.rs`

命令执行调度模块。

**单命令执行**：
- `handle_command()`：完整流程（BROM 安全绕过 → auto dump preloader → 加载 EMI → upload_da → 执行命令 → jump_bl 复位）
- `execute_single_command()`：执行单个 DA 命令（printgpt/read/write/erase/vbmeta/reset/unlock/lock/enable-adb-on-da）

**批量命令执行**：
- `handle_commands()`：批量执行多个命令，保持 DA 会话（流程同 handle_command，但循环执行 commands）

**具体命令实现**：
- `cmd_printgpt()`：读 GPT → 显示 EMMC 信息 → 生成 scatter.txt → 可选保存 gpt_debug.bin
- `cmd_dumpbrom()`：通过 dump payload 提取 BROM
- `cmd_read()`：读分区到文件
- `cmd_write()`：写文件到分区（可选校验）
- `cmd_erase()`：擦除分区
- `cmd_vbmeta()`：修补 vbmeta
- `cmd_reset()`：重启设备（jump_bl）
- `cmd_unlock()` / `cmd_lock()`：解锁/锁定 Bootloader
- `print_help()`：打印帮助信息

***

### 12. src/driver.rs

**路径**: `d:\test\ZybClient\src\driver.rs`

Windows 驱动安装模块。

**功能**：
- `install_winusb_driver()`：完整驱动安装流程（查找 MediaTek COM 端口 → 关闭 Watchdog → pnputil 安装 INF → certutil 导入证书）
- `find_mediatek_com_port()`：通过 serialport 和 wmic 查找 MediaTek COM 端口
- `disable_watchdog_serial()`：通过串口发送 0xA0 关闭 Watchdog
- `install_driver_inf()`：pnputil 安装 INF 文件
- `install_certificates()`：certutil 导入 Root 和 TrustedPublisher 证书
- `check_driver()`：检查 WinUSB 驱动是否已安装（枚举 pnputil 驱动）

***

### 13. src/usb_diag.rs

**路径**: `d:\test\ZybClient\src\usb_diag.rs`

USB 诊断模块。

**UsbDiagState**：NoDevice / WrongDriver / Preloader / Brom / EndpointError / HandshakeFailed

**功能**：
- `scan_mediatek_devices()`：扫描所有 MediaTek USB 设备，检查驱动状态
- `diagnose_connection()`：诊断 USB 连接状态
- `print_connection_hint()`：打印连接提示（BROM/Preloader 模式进入方法）
- `enumerate_usb_devices()`：枚举所有 USB 设备
- `diagnose_and_report()`：综合诊断报告

***

### 14. src/paths.rs

**路径**: `d:\test\ZybClient\src\paths.rs`

路径解析工具。

- `exe_relative_path()`：获取相对于资源根目录的路径，支持开发模式（target/debug/）和发布模式（exe 与资源同目录）

***

## 模块关系图

```
main.rs
  ├── cli.rs（CLI 参数）
  ├── config.rs（设备/芯片配置）
  ├── commands.rs（命令调度）
  │     └── da_xflash.rs（XFlash 协议）
  │           ├── da_partition.rs（分区操作）
  │           └── da_extension.rs（DA 修补/扩展）
  └── preloader.rs（BROM/Preloader 协议）
        ├── usb.rs（USB 通信）
        └── kamakiri2.rs（漏洞利用，作为 Preloader 的 impl）
```

## 协议对齐要点

1. **Echo 协议**：1 字节命令用 `echo_1byte()`（write + read_exact 1 字节比较），参数用 `device.write()`（大端），不 echo 参数
2. **rword/rdword**：BROM 阶段使用 big-endian（对齐 Python `>H`/`>I`），XFlash 阶段使用 little-endian
3. **send_da**：echo(0xD7) → write(addr/size/sig_len 大端) → rword() → 64 字节分块上传 → ZLP → 读 checksum + status2
4. **brom_register_access**：echo(0xDA) → write(mode/addr/len 大端) → 读状态 → 读/写数据
5. **upload_data**：64 字节分块（对齐 Python maxinsize=64），每 0x2000 字节发 ZLP
