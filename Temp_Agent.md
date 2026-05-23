# Temp_Agent.md — ZybFlashTool 会话上下文

> 写入时间：2025-05-18
> 本文件供下一会话恢复时使用，包含所有修改记录、项目状态和关键协议细节。

---

## 项目基本信息

- **名称**：ZybFlashTool / mtkclient-rs
- **路径**：`D:\test\ZybClient`
- **语言**：Rust 2024 edition
- **Cargo name**：`mtkclient-rs`
- **关键依赖**：rusb, libusb1-sys, log, env_logger, colored, clap, serialport, sha2, aes, cbc
- **构建**：`cargo build`（编译通过，0 error 0 warning）
- **禁止**：`cargo run`（需要连接设备）

---

## 文件结构（核心）

```
src/
  main.rs          — 入口，命令行解析，命令路由
  commands.rs      — 命令分发和主流程控制（printgpt, read, write, erase, vbmeta, unlock, lock, enable-adb）
  kamakiri2.rs     — Kamakiri2 exploit：da_read/da_write/inject_payload/dump_preloader/bypass_security
  da_xflash.rs     — DA/XFlash 协议层：DA 上传/patch/EMI/extensions/read/write/erase/seccfg
  preloader.rs     — BROM 协议层：echo/hw_code/看门狗/handshake/send_da/jump_da/upload_data/brom_register_access
  config.rs        — 芯片常量配置（所有芯片的寄存器地址、payload路径、blacklist等）
  cli.rs           — clap 命令行参数解析
  usb.rs           — USB 通信层（libusb FFI），设备打开/读写/控制传输
  driver.rs        — WinUSB 驱动安装
  paths.rs         — 路径辅助函数
```

### 拆分说明
- `preloader.rs`（约 315 行）：只保留 BROM 协议命令
- `kamakiri2.rs`（约 425 行）：通过 `#[path = "kamakiri2.rs"] mod kamakiri2;` 在 `preloader.rs` 底部引入，扩展 `impl Preloader`
- `da_xflash.rs`（约 2900 行）：DA/XFlash 协议，最大的文件

---

## 本次会话完整修改记录

### 会话 1：初始 6 个任务修复
1. **清理 warn 未使用导入**：`src/commands.rs` 移除 `warn`
2. **修复 GPT 自动加载**：三个函数改用 `self.read_gpt()` 替代手动解析
3. **修复 run_kamakiri2 读取逻辑**：ACK 后读 4 字节小端长度 → 读完整数据
4. **Phase 2 改用 dump_preloader_payload**：`run_kamakiri2()` → `dump_preloader_payload(false)`
5. **DA 命令自动复位**：`jump_bl()` 在 DA 命令后自动调用
6. **移除重复复位**：`cmd_printgpt` 无 `jump_bl()`，无需修改

### 会话 2：mtkclient v2.0.1 → v2.1.4.1 差异对比
- 对比两个 Python 版本，发现 30 个新增文件、14 个删除文件
- 写入 `Mtkclient_Commit.md`

### 会话 3：应用 v2.1.4.1 中有价值的 3 项更新
1. **USB 包大小动态化**：`UsbDevice` 新增 `ep_out_max_packet_size` 字段，从 USB 描述符解析
2. **ACK 芯片差异化**：`da_xflash.rs` `ack()` 添加 MT6781 单包发送注释
3. **readflash 超时异常清理**：`readflash_data` 重写为完整 15 步协议序列，try/finally 模式

### 会话 4：DA 会话保持（批量模式）
- `handle_commands` 批量执行函数，`execute_single_command` 提取
- `main.rs` `parse_sub_commands` 通过已知 DA 命令关键词列表智能解析批量命令
- `print_help` 更新

### 会话 5：SEND_DA failed 修复（多次迭代）
- 最初认为是设备重枚举问题，添加了 `reopen_device`
- `reopen_device` 的 handshake 失败（device read error at byte 0 / write err -7）
- 根因：Python `connect()` 只是重新连接 USB，不重新握手；Rust `reopen_device` 执行完整 handshake
- 修复：移除 `reopen_device` 调用，继续使用同一 USB 句柄

### 会话 6：新增 enable-adb-on-da 命令
- 通过 `SET_META_BOOT_MODE(0x020006)` 设置 meta+usb 模式
- `boot_to` 重启到 meta 模式实现 ADB 启用

### 会话 7：重写 echo 函数对齐 Python
- 旧版 echo：先 drain 64 字节缓冲区 → 发数据+ZLP → 读响应+retry
- Python echo：发送 → 读等长响应 → 比较一致就返回 true
- 修复：完全重写 echo，移除 drain、ZLP、retry

### 会话 8：查看新版 mtkclient DAA/SLA 校验改动
- v2.1.4.1 的 `handle_sla` 新增 Lake/Tides/Moon（小米 Redmi 14C）硬编码签名绕过
- 新增 Motorola/Lamu 系列硬编码签名 + Motorola 专用 RSA key
- DA2 patch 新增 `moto_disable_sla` 补丁

### 会话 9：代码审查 — Rust vs Python 协议对比
**发现 5 个问题：**
1. `setup_storage` 多余 DEVICE_CTRL 调用 → 简化为 2 步
2. `readflash_data` 命令号错误（`CMD_BASIC_READ_DATA = 0x010005` 正确，之前改成了 `0x000F0005` 错误）→ 改回 `0x010005`
3. `boot_to` 空 data 时多余 ZLP → 移除
4. `upload_data` sleep 120ms → 改为 35ms（对齐 Python `time.sleep(0.035)`）
5. `cmd_write_data` NandExtension 字段语义不一致 → 低风险未改

**MT6768 配置修复**（7 个字段与 Python 不一致）：
- `gcpu_base`: 0x10210000 → 0x10050000
- `send_ptr`: (0x1028B4, 0xF5AC) → (0x10286C, 0xC190)
- `ctrl_buffer`: 0x001032F0 → 0x00102A28
- `cmd_handler`: 0x000102C3 → 0x0000CF15
- `brom_register_access`: (0xF9C0, 0xFA78) → (0xC598, 0xC650)
- `meid_addr`: 0x102B08 → 0x102AF8
- `blacklist`: [(0x102870,0x0), (0x107070,0x0)] → [(0x10282C,0x0), (0x00105994,0)]

### 会话 10：echo 协议反复折腾
- 最初误判 echo 单字节命令应该发 4 字节大端 → 错误修改 → 恢复
- Python `echo(bytes)` 对 bytes 类型直接发送（1 字节就是 1 字节）
- Python `echo(int)` 才做 `pack(">I", int)` → 4 字节
- **结论**：Rust `echo(&[0xD7])` 发 1 字节是正确的，所有单字节命令保持原样
- 添加 `echo_debug()` 公开方法用于诊断

### 会话 11：dump_preloader_payload 后 echo 失效根因分析
- **根因**：`UsbDevice::read()` 在 `transferred=0` 时 sleep 10ms 重试，dump 最后阶段导致 bulk IN 端点状态异常
- **修复**：`dump_preloader_payload` 完成后调用 `clear_halt_in()` 复位 bulk IN 端点
- **诊断**：添加 `echo_debug` 验证代码和 echo 内部 debug 日志
- 在 `commands.rs` Phase 2 dump 成功后添加 echo(0xFD) 验证

### 会话 12：文件结构优化
- `preloader.rs` 拆分：BROM 协议命令保留，kamakiri2 相关移入 `kamakiri2.rs`
- `kamakiri2.rs` 通过 `#[path = "kamakiri2.rs"] mod kamakiri2;` 在 `preloader.rs` 底部引入
- 两个文件都扩展 `impl Preloader`
- 编译通过，0 warnings

---

## 关键函数和协议细节

### BROM 阶段
- `echo(&[0xD8])` → GET_TARGET_CONFIG，获取芯片信息
- `echo(&[0xD6])` → jump_bl，重启设备恢复 BROM 状态
- `bypass_security()` → 注入 generic_patcher_payload.bin 绕过 SLA/DAA
- `kamakiri2` → 注入 loader payload，exploit 后读 preloader

### DA 加载流程（upload_da）
1. `upload_da1()` → 发送 DA Stage1 → jump_da → 等待 0xC0 同步 → xflash_sync → setup_env → setup_hw_init
2. 发送 EMI 数据（如果 conn_agent == "brom" 且有 EMI）
3. `upload_da2()` → boot_to Stage2
4. `get_sla_status()` → 检查 SLA
5. `reinit()` → 获取 RAM/芯片/EMMC 信息
6. `generate_da_extensions()` → boot_to(0x4FFF0000) → send_devctrl(0x0F0000) 验证 0xA1A2A3A4 → custom_set_storage

### XFlash 协议
- pack3: magic(4B) + data_type(4B) + length(4B) = 12 字节头
- CMD_MAGIC = 0xFEEEEEEF
- status()：读 12 字节头，读 length 字节数据
- xread_data()：同 status 但返回 Vec<u8>
- send_devctrl(cmd, param) → xsend(DEVICE_CTRL) → status → xsend(cmd) → status → send_param/ xread

### GPT 解析
- read_gpt()：send_devctrl(0x040007) 初始化 → readflash_data(0, 16384) → 解析 EFI PART
- GPT 在 LBA 1（偏移 0x200），但 readflash_data 从地址 0 读取，数据中包含完整 GPT
- 签名搜索：EFI PART at offset
- 分区表：part_entry_start_lba * 512，跳过可能的前导 4 字节状态包
- 分区条目：name(UTF-16LE at 56), first_lba(32), last_lba(40), unique_guid(16)

### dump_preloader_payload 协议
1. inject_payload(payload, 0xC1C2C3C4)
2. 读 4 字节小端长度
3. 循环读 512 字节块直到长度读完
4. 搜 MTK_BLOADER_INFO 提取文件名（offset + 0x1B 处开始，最多 0x30 字节）
5. dump 完成后 `clear_halt_in()` 复位 bulk IN 端点

### seccfg 版本
- V4：magic = 0x4D4D4D4D，头部 28 字节，尾部 SHA256 加密哈希（32 字节）
- V3：info_header = "AND_SECCFG_v\x00\x00\x00\x00"，44 字节头部
- 加密模式：SW (AES-256-CBC), V2/V3/V4 (AES-128-CBC)
- 离线模式：8MB 分区大小补齐
- C# 版正确行为：只修改 lock_state(0x0C)，不动 critical_lock_state/dm_verity(0x10)

### jump_bl
- echo(&[0xD6]) → rword() 读 status → 如果 status <= 0xFF 再读 status2 → 返回 true

### echo 协议
- Python `echo(bytes)` → 直接发送 bytes，读等长响应
- Python `echo(int)` → `pack(">I", int)` → 4 字节大端
- Rust `echo(&[0xD7])` 发 1 字节 ✅ 正确
- Rust `echo(&addr.to_be_bytes())` 发 4 字节大端 ✅ 正确
- `echo_debug()` 公开版本，带 debug 日志

### DA Extensions
- 模板：`mtkclient-2.0.1/mtkclient/payloads/da_x.bin`
- 搜索 DA2 中函数地址：register_devctrl, mmc_get_card, mmc_set_part_config, mmc_rpmb_send_command, ufshcd_queuecommand, g_ufs_hba
- 替换模板中的占位符：0x11111111, 0x22222222, etc.

### USB 端点
- EP_OUT=0x01 wMaxPacketSize 从设备描述符动态解析
- EP_IN=0x81
- `read()` 有 retry 逻辑：`transferred=0` 时 sleep 10ms 重试
- `write()` 发送空切片会触发 ZLP

### readflash_data 完整协议序列
1. xsend(READ_DATA=0x010005) = pack3(MAGIC, 1, 4) + READ_DATA
2. status()
3. send_param: storage(4) + parttype(4) + addr(8) + size(8) + NandExtension(32) = 56 字节
4. status()
5. xread() 循环 + ack() 直到读完
6. status()

---

## 编译验证

```
cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in ~16s
```

0 error, 0 warning.

---

## 注意事项

1. 不要运行 `cargo run`（需要设备连接）
2. 不要修改 `main.rs` 中的命令路由逻辑
3. `read_gpt()` 内部调用 `send_devctrl(0x040007)` + `readflash_data`，已包含 reinit 逻辑
4. DA 命令执行后必须调 `jump_bl()` 恢复 BROM 状态
5. `dump_preloader_payload` 是验证成功的 preloader 提取方法，优先使用
6. **echo 协议**：单字节命令发 1 字节，参数发 4 字节大端，不要改
7. **USB read**：dump 后需要 `clear_halt_in()` 复位端点
8. **upload_data sleep**：35ms（对齐 Python 0.035s）
9. **MT6768 配置**：已修复，与 Python brom_config.py 一致
10. **MT6771 配置**：原始即正确

---

## 最近会话完整修改记录（2026-05-22 起）

### 会话 13：USB 设备标识符管理重构
**日期**：2026-05-22
**文件**：`src/config.rs`, `src/usb.rs`, `src/main.rs`
**问题**：USB 设备的 VID/PID 在多处硬编码，分散在 config.rs 和 usb.rs 中，维护困难。
**解决方案**：
- `config.rs` 新增 `DeviceType` 枚举：Brom, Preloader, PreloaderVariant, Unknown
- `config.rs` 定义 `SUPPORTED_DEVICES` 配置表，统一管理所有支持的设备 VID/PID/类型/名称/描述
- `config.rs` 新增 `DeviceType::from_vid_pid()` 方法
- `usb.rs` `UsbDevice::new()` 改用 `SUPPORTED_DEVICES` 表遍历查找设备，不再硬编码 VID/PID
- `main.rs` `detect_mode()` 改用 `DeviceType::from_vid_pid()` 统一判断
- 消除了 `MEDIA_TEK_VID` 和多个 PID 常量的重复定义
**编译**：`cargo check` 通过

### 会话 14：配置中科大镜像源加速 Rust 工具链
**日期**：2026-05-22
**Cargo 镜像源**：`C:\Users\wqy\.cargo\config.toml`
```toml
[source.ustc]
registry = "https://mirrors.ustc.edu.cn/crates.io-index"
[source.crates-io]
replace-with = "ustc"
```
**rustup 镜像源**：设置系统环境变量
- `RUSTUP_DIST_SERVER = "https://mirrors.ustc.edu.cn/rust-static"`
- `RUSTUP_UPDATE_ROOT = "https://mirrors.ustc.edu.cn/rust-static/rustup"`
**升级 Cargo**：使用 `rustup update` 命令，先清理旧配置再重新安装工具链

### 会话 15：代码审查 — 重构建议清单
**日期**：2026-05-22
**分析内容**：对比新版 mtkclient 2.0.1 架构，提出 6+ 项重构建议：
1. **GPT 分区解析代码去重**（高优先级）— 建议创建独立 `gpt.rs` 模块
2. **seccfg 操作代码去重**（高优先级）— 建议创建 `seccfg.rs`，统一 lock/unlock 逻辑
3. **拆分大文件 da_xflash.rs**（中优先级）— 建议拆分为 crypto/sej.rs, patch.rs, da/ 等
4. **命令处理逻辑去重**（中优先级）— 提取 `prepare_brom_and_da()` 共享函数
5. **引入常量定义**（低优先级）— 创建 `constants.rs` 统一管理魔术数字
6. **统一二进制搜索工具**（低优先级）— 封装通用搜索函数

### 会话 16：项目文件索引文档生成
**日期**：2026-05-22
**文件**：`files_doc.md`
**内容**：
- 项目概述和结构说明
- 核心文件索引（8 个主要模块）：Cargo.toml, main.rs, cli.rs, config.rs, usb.rs, preloader.rs, driver.rs, usb_diag.rs
- 每个文件包含功能说明和关键代码片段
- 项目总结

### 会话 17：增强 USB 通信日志系统
**日期**：2026-05-22
**文件**：`src/cli.rs`, `src/usb.rs`, `src/main.rs`
**要求**：实现和 Python mtkclient 同级别的 USB 调试日志，与 `--debugmode` 并列独立控制。

**实现内容**：
1. **cli.rs 新增 `--usb-log` 参数**（第 48-53 行）
   ```rust
   #[arg(long = "usb-log", default_value_t = false)]
   pub usb_log: bool,
   ```

2. **usb.rs 新增 USB trace 系统**：
   - `USB_LOG_ENABLED` 全局 AtomicBool 开关
   - `USB_LOG_FILE` 静态 Mutex<File> 日志文件
   - `set_usb_log_enabled(bool)` 初始化函数
   - `usb_trace(direction, func_info, data)` 核心日志函数
   - 格式对齐 Python usb_debug.log：`[HH:MM:SS.mmm] [TX/RX] [函数名::行号] hex_data`
   
3. **在所有 USB 通信点插入 trace 调用**：
   - `write()` → TX trace（含 ZLP）
   - `read()` → RX trace（实际读取的数据）
   - `read_exact()` → RX trace
   - `ctrl_transfer_in()` → RX trace
   - `ctrl_transfer_out()` → TX trace
   - `do_handshake()` → echo write/read trace

4. **main.rs 初始化**（第 142-143 行）：
   ```rust
   usb::set_usb_log_enabled(cli.usb_log);
   ```

**使用方式**：
```bash
# 启用 USB 通信日志（输出到 usb_debug.log）
mtkclient-rs --usb-log printgpt

# 与 --debugmode 独立使用
mtkclient-rs --usb-log --debugmode printgpt

# 默认关闭，不产生日志文件
mtkclient-rs printgpt
```

**日志示例**：
```
[14:23:30.854] [TX] [src\usb.rs:270] d7
[14:23:30.854] [RX] [src\usb.rs:380] d7
[14:23:30.854] [TX] [src\usb.rs:485] a0 0a 50 05
```

**编译验证**：`cargo check` 通过，0 error

---

## 新增文件结构

```
files_doc.md           — 项目文件索引文档（2026-05-22 生成）
usb_debug.log          — USB 通信追踪日志（--usb-log 启用时生成）
```

---

## 新增配置信息

### 中科大镜像源
- Cargo：`C:\Users\wqy\.cargo\config.toml` 配置 ustc source
- rustup：环境变量 `RUSTUP_DIST_SERVER` 和 `RUSTUP_UPDATE_ROOT`

### USB Trace 日志格式
- 时间：`[HH:MM:SS.mmm]`（UTC+8 东八区）
- 方向：`[TX]` 发送 / `[RX]` 接收
- 位置：`[文件路径:行号]`
- 数据：hex 格式（空格分隔）
- 独立于 `--debugmode`，默认关闭

### 会话 18：增强 Python mtkclient USB 调试日志
**日期**：2026-05-23
**涉及文件**：
- `mtkclient-2.0.1/mtkclient/Library/Connection/usblib.py`
- `mtkclient-2.0.1/mtkclient/Library/Port.py`
- `mtkclient-2.0.1/mtkclient/Library/mtk_preloader.py`
- `mtkclient-2.0.1/mtkclient/Library/Exploit/kamakiri2.py`

**改动内容**：

1. **usblib.py — 修复 usbwrite/write 重复日志**：
   - `write()` 新增 `do_log=False` 参数，默认不打印 TX 日志（避免被 `usbwrite()` 重复记录）
   - `usbwrite()` 新增 `cmd_name` 参数，支持自定义 TX 标签（如 `TX:SEND_DA`）
   
2. **Port.py — echo() 函数增强**：
   - 新增 `cmd_name` 参数，传递给 `usbwrite()` 用于标记命令名
   - echo TX 日志由 `usbwrite()` 统一记录（通过 cmd_name 标记）
   - 新增 ECHO:RX 日志：`[ECHO] RX: hex_data`
   - 新增 ECHO:MISMATCH 日志：`[ECHO] MISMATCH TX=hex RX=hex`
   - 所有 trace 代码用 `DEBUG_USB` 开关控制

3. **mtk_preloader.py — echo 调用添加命令名标记**：
   - 所有 `echo()` 调用添加 `cmd_name` 参数，如：
     - `echo(GET_HW_CODE, cmd_name="GET_HW_CODE")`
     - `echo(0xD7, cmd_name="SEND_DA")`
     - `echo(addr, cmd_name="addr")`
     - `echo(len, cmd_name="len")`
   - `brom_register_access()` 添加入口/出口 trace：
     - 入口：`[BROM_REG] mode=X addr=0xXXXX len=X`
     - 出口：`[BROM_REG] result=OK, got X bytes`
   - 导入 `DEBUG_USB` 和 `usb_debug_log`

4. **kamakiri2.py — da_read_write 添加 trace**：
   - 入口：`[DA_RW] addr=0xXXXX len=X write=X`（write=1 表示写操作，0 表示读操作）
   - 出口：`[DA_RW] done`
   - 导入 `DEBUG_USB` 和 `usb_debug_log`

**日志格式示例**：
```
[12:34:56.789] [TX:SEND_DA] [usblib.py:664::usbwrite] d7
[12:34:56.790] [ECHO:RX] [usblib.py:66::usb_debug_log] [ECHO] RX: d7
[12:34:56.791] [TX:addr] [usblib.py:664::usbwrite] 00000040
[12:34:56.792] [BROM_REG] [usblib.py:61::usb_debug_log] [BROM_REG] mode=0 addr=0x0 len=64
[12:34:56.793] [DA_RW] [usblib.py:61::usb_debug_log] [DA_RW] addr=0x100000 len=64 write=0
[12:34:56.800] [DA_RW] [usblib.py:61::usb_debug_log] [DA_RW] done
```

**约束**：
- 不改变任何功能逻辑，只添加日志
- 所有 trace 代码均受 `DEBUG_USB` 开关控制
- 保持现有日志格式：`[HH:MM:SS.mmm] [TAG] [TX/RX] hex_data`

**已修改的 echo 调用列表**（mtk_preloader.py）：
- `GET_HW_CODE`, `CMD_READ16_A2`, `READ16/READ32`, `WRITE16/WRITE32`
- `JUMP_BL`, `JUMP_TO_PARTITION`, `SEND_PARTITION_DATA`
- `GET_TARGET_CONFIG`, `JUMP_DA`, `JUMP_DA64`
- `UART1_LOG_EN`, `UART1_SET_BAUDRATE`
- `SEND_CERT`, `SEND_AUTH`, `SLA`
- `BROM_DEBUGLOG`, `GET_BROM_LOG_NEW`
- `brom_register_access`（含 BROM_REG trace）
- `SEND_DA`（含 addr/len/sig_len 标记）

---

## 会话 27：echo 协议完全对齐 Python（2026-05-23）

**根因**：重建的 preloader.rs 中 echo 发送 1 字节，但 Python Port.echo() 对 int 参数执行 `pack(">I", int)` → 发送 4 字节。
- 日志显示：`echo(&[0xC8])` 期望回 C8 但收到 00 → 协议完全错位
- `get_target_config` 读 4 字节但 Python 读 6 字节
- `brom_register_access` echo 序列混乱

**修改**：
- `preloader.rs` 完全重写：
  - `sendcmd(0xFD)` → `echo(&0xFD_u32.to_be_bytes())`（4 字节）
  - 新增 `echo32(u32)` 方法发送 4 字节 echo
  - `brom_register_access` 中 4 个 echo 全部用 `echo32()`（mode, address, length 均为 4 字节）
  - `get_target_config` 读 6 字节：rdword() 读 4 字节 + rword() 读 2 字节
  - 新增 `TargetConfig::from_raw_u64(u64)` 方法
  - `init()` 恢复使用 `device.do_handshake()`（BROM 4 字节握手）
- `config.rs`：TargetConfig 新增 `from_raw_u64`，旧 `from_raw` 标记 `#[allow(dead_code)]`

**关键发现**：
1. Python `echo(0xC8)` → `pack(">I", 0xC8)` → `b'\x00\x00\x00\xC8'`（4 字节）
2. Python `brom_register_access` 的 mode/address/length 都通过 `echo(pack(">I", val))` 发送（4 字节 echo，不是 write）
3. Python `get_target_config` 读 6 字节，不是 4 字节
4. BROM 握手 `do_handshake` 发送 4 字节 `0xA0A0A0A0`，设备回显 `0x5F5F5F5F`（取反）

## 会话 28：sendcmd 改回 1 字节 + brom_register_access 逐字节 echo（2026-05-23）

**根因**：BROM 模式（reopen USB 后）对命令 echo 的响应是 **1 字节**，不是 4 字节。
- `echo(&[0xD7])` 发 4 字节 → 设备回 0 字节 → SEND_DA 失败
- `echo(&[0xFD])` 发 4 字节 → 设备回 4 字节 ✅（preloader dump 成功因为 get_hw_code 用 4 字节）
-  reopen USB 后设备回到 BROM 模式，但 echo 协议不同

**修改**：
- `sendcmd(cmd: u8)` → `echo(&[cmd])`（1 字节）
- `echo32()` 方法删除
- `brom_register_access`：
  - `echo(&[0xDA])`（1 字节命令）
  - `echo(&mode.to_be_bytes())`（4 字节，单次 echo）
  - `echo(&address.to_be_bytes())`（4 字节，单次 echo）
  - `echo(&length.to_be_bytes())`（4 字节，单次 echo）
- `send_da`/`jump_da`/`jump_bl`：`echo(&[0xD7])`/`echo(&[0xD5])`/`echo(&[0xD8])`（1 字节）
- `get_hw_code`/`get_target_config`：`sendcmd(0xFD)`/`sendcmd(0xC8)`（1 字节）

**关键理解**：
- `sendcmd` 发送的是 **命令字节**（如 0xFD, 0xC8, 0xD7），设备 1 字节回显
- `brom_register_access` 的 mode/address/length 是 **参数**，用 `pack(">I", val)` 4 字节 echo（Python Port.echo 对 bytes 参数直接发送整个 bytes）
- 两种 echo 模式共存：1 字节命令 + 4 字节参数


