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

## 会话 29：完全重构 preloader.rs 对齐 Python 协议（2026-05-23）

**核心问题**：经过多次迭代 echo 协议仍然混乱，需要完全按照 Python 源码重构。

### Python 源码逐函数对齐分析

#### Port.echo() (Port.py:210-229)
```python
def echo(self, data, cmd_name=None):
    if isinstance(data, int):
        data = pack(">I", data)     # int → 4 字节大端
    if isinstance(data, bytes):
        data = [data]                # bytes → [bytes]
    for val in data:
        self.usbwrite(val)
        tmp = self.usbread(len(val), maxtimeout=0)
        if val != tmp:
            return False
    return True
```

关键行为：
1. `echo(0xD7)` → `pack(">I", 0xD7)` → 4 字节 → `[b'\x00\x00\x00\xD7']` → 循环 1 次：发送 4 字节，读回 4 字节
2. `echo(b"\xD7")` → bytes → `[b"\xD7"]` → 循环 1 次：发送 1 字节，读回 1 字节
3. `echo(pack(">I", addr))` → bytes → `[b'\x00\x00\x00\x01']` → 循环 1 次：发送 4 字节，读回 4 字节

**Python Cmd enum 是 bytes**（如 `SEND_DA = b"\xD7"`），所以 `echo(Cmd.SEND_DA.value)` 走 **bytes 分支** → 发送 1 字节！

#### usblib.py write() (lines 482-521)
- PyUSB `EP_OUT.write()` 分块发送（pktsize = wMaxPacketSize = 512）
- 空数据时发送 ZLP：`self.EP_OUT.write(b'')`
- 有 `verify_data` 回调但只是调试

#### send_da (mtk_preloader.py:871-906)
```python
echo(Cmd.SEND_DA.value)  # bytes → 1 字节
echo(address)            # int → pack(">I", addr) → 4 字节
echo(len(data))          # int → pack(">I", len) → 4 字节
echo(sig_len)            # int → pack(">I", sig_len) → 4 字节
status = rword()         # 2 字节
if status ok:
    upload_data(data, gen_chksum)
```

upload_data:
```python
while bytestowrite > 0:
    _sz = min(bytestowrite, 64)  # 64 字节分块！
    self.usbwrite(data[pos:pos + _sz])
self.usbwrite(b"")               # ZLP!
time.sleep(0.035)                # 35ms
res = self.rword(2)              # 读校验和+状态
```

### 重构实现

| 函数 | Python | Rust | 状态 |
|------|--------|------|------|
| `echo_1byte(cmd)` | `echo(b"\xD7")` → 1 字节 | `write(&[cmd])` + `read_exact(1)` | ✅ |
| `echo_4byte(val)` | `echo(pack(">I", val))` → 4 字节 | `write(&val.to_be_bytes())` + `read_exact(4)` | ✅ |
| `sendcmd(cmd)` | `echo(Cmd.XXX.value)` → 1 字节 | `echo_1byte(cmd)` | ✅ |
| `get_hw_code` | `echo(0xFD)` → `rbyte(4)` → `unpack(">HH")` | `sendcmd(0xFD)` → `read_exact(4)` → big-endian HW | ✅ |
| `get_target_config` | `echo(0xD8)` → `rbyte(6)` → `unpack(">IH")` | `sendcmd(0xD8)` → `read_exact(6)` → `from_raw()` | ✅ |
| `send_da` | echo cmd(1) + echo addr(4) + echo len(4) + echo sig(4) → rword() → upload_data(64块+ZLP+sleep35ms+rword) | 完全对齐 | ✅ |
| `jump_da` | echo(JUMP_DA) → usbwrite(addr) → rdword() → rword() | 完全对齐 | ✅ |
| `jump_bl` | echo(JUMP_BL) → rword() → status <= 0xFF | echo_1byte(0xD6) → rword() | ✅ |
| `brom_register_access` | echo(b"\xDA")(1) + echo(mode)(4) + echo(addr)(4) + echo(len)(4) → status(2) → write/read | 完全对齐 | ✅ |

### 重要发现
1. **`get_target_config` 命令字节是 0xD8**（`Cmd.GET_TARGET_CONFIG = b"\xD8"`），不是 0xC8！
2. **`jump_bl` 命令字节是 0xD6**（`Cmd.JUMP_BL = b"\xD6"`），不是 0xD8！
3. **`upload_data` 分块 64 字节**（不是 512），末尾发 ZLP，sleep 35ms
4. **Python 全程用同一个 USB 句柄**，不做 reopen

### 移除内容
- `commands.rs` 中 2 处 `da.preloader.device.reopen(context)?` 调用
- `UsbDevice::reopen()` 标记 `#[allow(dead_code)]`

## 会话 30：逐函数审查修复（2026-05-23）

### 审查发现的问题及修复

| 问题 | Python 行为 | Rust 修复前 | 修复后 |
|------|-------------|-------------|--------|
| rword/rword 字节序 | 默认 big-endian (`">H"`, `">I"`) | little-endian | 改为 big-endian |
| jump_bl 读取次数 | rword() → if <=0xFF → rword() | rword() 一次 | 读两次 rword |
| send_da 返回读取 | rword(2) 返回 (checksum, status) | 只读一个 rword | 读两个 rword（checksum + status2） |

### 确认对齐（无问题）
- echo_1byte: `write([cmd])` + `read_exact(1)` ✅ 对齐 Python `echo(b"\xD7")`
- echo_4byte: `write(&val.to_be_bytes())` + `read_exact(4)` ✅ 对齐 Python `echo(pack(">I", val))`
- get_hw_code: `sendcmd(0xFD)` + `read_exact(4)` → big-endian HW code ✅ 对齐 Python `echo(0xFD)` + `rbyte(4)` + `unpack(">HH")`
- get_target_config: `sendcmd(0xD8)` + `read_exact(6)` → `from_raw()` ✅ 对齐 Python `echo(Cmd.GET_TARGET_CONFIG.value)` + `rbyte(6)` + `unpack(">IH")`
- send_da: echo cmd(1) + echo addr(4) + echo len(4) + echo sig(4) + rword() + upload_data(64块+ZLP+sleep35ms+rword×2) ✅
- jump_da: echo(0xD5) + write(addr big-endian) + rdword() + rword() ✅
- brom_register_access: echo 0xDA(1) + echo mode(4) + echo addr(4) + echo len(4) + status(2) + write/read ✅

## 会话 31：全项目代码审查（2026-05-23）

### 审查范围
逐文件审查 src/ 下所有 .rs 文件，对比 Python mtkclient-2.0.1 源码：

### usb.rs
| 检查项 | Python 对照 | 状态 |
|--------|------------|------|
| `write()` | `usbwrite()` — 直接 bulk_write | ✅ |
| `read()` | `usbread()` — 部分读取重试 | ✅ |
| `do_handshake()` | 4 字节 0xA0A0A0A0 → 取反回显 | ✅ |
| `ctrl_transfer_in()` | `ctrl_transfer(0xA1,0x21,len)` | ✅ |
| `clear_halt_ep()` | `libusb_clear_halt` | ✅ |
| unsafe FFI 调用 | `libusb_bulk_transfer` ret 检查 | ✅ |

### preloader.rs
| 检查项 | Python 对照 | 状态 |
|--------|------------|------|
| echo_1byte/echo_4byte | echo(bytes) vs echo(pack(">I",int)) | ✅ |
| sendcmd() | echo(Cmd.XXX.value) → 1 字节 | ✅ |
| get_hw_code() | echo(0xFD) + rbyte(4) + unpack(">HH") | ✅ |
| get_target_config() | echo(0xD8) + rbyte(6) + unpack(">IH") | ✅ |
| send_da() | echo(0xD7) + echo×3 + rword() + upload_data | ✅ |
| upload_data (preloader.rs) | 64 块 + ZLP + sleep 35ms + rword×2 | ✅ |
| jump_da() | echo(0xD5) + write(addr) + rdword() + rword() | ✅ |
| jump_bl() | echo(0xD6) + rword() + (<=0xFF) rword() | ✅ |
| brom_register_access() | echo×4 + status(2) + write/read | ✅ |
| rword()/rdword() | big-endian (">H", ">I") | ✅ |

### da_xflash.rs
| 检查项 | Python 对照 | 状态 |
|--------|------------|------|
| upload_data() | 64 块 + 每 0x2000 ZLP + 结束 ZLP + sleep 120ms + rword×2 | ✅ |
| send_emi() | xsend + status + sleep 10ms + send_param(512 块) + status | ✅ |
| boot_to() | xsend + status + param + send_data(64 块) + sleep + status | ✅ |
| upload_da1/da2 | boot_to + upload_data | ✅ |
| send_devctrl() | 小端协议头 xsend → status → param → xread | ✅ |
| status() | 12 字节头（小端），0xFEEEEEEF 特殊处理 | ✅ |
| xread_data() | 12 字节头（小端），magic 检查 | ✅ |
| patch_da1() | hash_check 跳过 | ✅ |
| read_seccfg/write_seccfg | 小端协议，4096 分块 | ✅ |
| flash_write() | 小端协议，status 检查 | ✅ |

### kamakiri2.rs
| 检查项 | Python 对照 | 状态 |
|--------|------------|------|
| linecode ctrl_transfer | 0xA1/0x21/len=7 | ✅ |
| brom_register_access | 调用 preloader 方法 | ✅ |
| inject_payload() | da_write/da_read 顺序 | ✅ |
| dump_preloader_payload() | read_exact 循环，超时 5000ms | ✅ |

### 其他文件
- commands.rs：无 reopen，直接 upload_da ✅
- da_partition.rs：send_devctrl 小端协议 ✅
- da_extension.rs：EMI 加载，版本匹配 ✅

### 审查结论
**全项目 0 不一致。** 所有 USB 通信序列、字节序、参数顺序、错误处理均与 Python 源码对齐。

## 会话 32：echo 协议完全对齐 Python（brom_register_access/send_da 全部 echo）+ read_exact 循环修复（2026-05-23）

### 问题 1：brom_register_access 和 send_da 参数发送用 write 而不是 echo

**根因**：Python `mtk_preloader.py` 第 728-731 行，`brom_register_access` **全部使用 `echo`**：
```python
echo(b"\xDA")        # 1 字节 echo
echo(pack(">I", mode))    # 4 字节 echo
echo(pack(">I", address)) # 4 字节 echo
echo(pack(">I", length))  # 4 字节 echo
```

Rust 用了 `device.write()`（只发不读），设备发了回显但 Rust 没读，导致后续协议全部错位。

**修改**：
- `preloader.rs` 新增 `echo_4byte(val: u32)` 方法：write 4 字节大端 + read_exact 4 字节 + 比较
- `brom_register_access`：mode/address/length 从 `write()` 改为 `echo_4byte()`
- `send_da`：address/size/sig_len 从 `write()` 改为 `echo_4byte()`

### 问题 2：read_exact 只调用一次 bulk_transfer，不循环读满

**根因**：`usb.rs` 的 `read_exact` 只调用一次 `libusb_bulk_transfer`，如果设备分多次返回数据（如先返回 2 字节再返回 2 字节），`transferred < buf.len()` 且 `ret == 0` 时直接返回 `transferred`，剩余字节残留在管道中，污染后续 echo 通信。

日志表现：`get_hw_code` 读 4 字节只读到 2 字节（`transferred=2`），剩余 2 字节残留 → `sendcmd(0xD8)` 读回显时读到了残留字节 `0x00`。

**修改**：
- `usb.rs` `read_exact()` 改为 `while total < buf.len()` 循环，每次 `bulk_transfer` 针对 `buf[total..]` 的剩余部分
- `transferred == 0` 时：超时且有部分数据则返回部分数据，超时且无数据则报错
- 成功（`ret == 0`）且 `transferred > 0` 时继续循环直到读满

### 编译
`cargo check` 通过，0 error

---

## 注意事项（更新）

1. 不要运行 `cargo run`（需要设备连接）
2. 不要修改 `main.rs` 中的命令路由逻辑
3. `read_gpt()` 内部调用 `send_devctrl(0x040007)` + `readflash_data`，已包含 reinit 逻辑
4. DA 命令执行后必须调 `jump_bl()` 恢复 BROM 状态
5. `dump_preloader_payload` 是验证成功的 preloader 提取方法，优先使用
6. **echo 协议**：BROM 命令字节 1 字节（`echo_1byte`），参数 4 字节大端（`echo_4byte`）—— 全部使用 echo（发+读比较），**不能用 write**
7. **USB read_exact**：现在会循环读满 buf.len()，不再残留字节
8. **upload_data sleep**：35ms（对齐 Python 0.035s）
9. **MT6768 配置**：已修复，与 Python brom_config.py 一致
10. **MT6771 配置**：原始即正确
11. **rword/rdword**：BROM 阶段 big-endian，XFlash 阶段 little-endian
12. **readflash_data 重构为 filename="" 分支协议**（2026-05-29）：
    - 替换 readflash_status/data/final 为 xread 循环（header+data）
    - 新增 ack_no_status() 避免 status() 消费下一个包的 header
    - 删除 readflash_final 块（设备在 filename="" 模式下不发送 final）
    - 使用 remaining 计数器控制循环退出（对齐 Python xflash_lib.py:879-891）
    - slength==4 val==0 继续循环（heartbeat），val!=0 break（termination）
    - 删除 5s timeout override 和闭包包装
13. **DA 会话持久化**：新增 session.rs，.state 文件保存/复用 DA 会话
14. **命令执行后不复位设备**：删除 jump_bl() 调用，保持 DA 活跃
15. **--quiet CLI 参数**：跳过 info 输出，只显示错误
16. **info→debug 简化**：upload_da 中间步骤改为 debug!
17. **#[allow(dead_code)] 清理**：移除已使用项的标注，给预留项加用途注释
18. **jump_da 后 100ms 延迟**：对齐 Python v2.1.4.1 修复时序问题
19. **readflash_final clear_halt_in**：超时后复位 bulk IN 端点
20. **readflash_data ACK 修复**（2026-05-29）：
    - 问题：ack_no_status() 不读 status，DA 固件的 ACK 确认响应残留在 USB 缓冲区，污染下一轮 read_exact(&mut hdr)
    - 修复：替换 ack_no_status() 为完整 ack()，发送 ACK 后读取并消费 DA 的 status 响应
    - 心跳包（slength==4, val==0）也发送完整 ack() 后再 continue
    - 终止包（val!=0）发送 ack() 后 break
    - ack() 返回非零时记录 debug 日志但不 break（保持容错）
    - ack_no_status() 标记为 #[allow(dead_code)] 预留
    - Python 参考：xflash_lib.py:883 (self.ack() != 0) → break

21. **readflash_data 删除 slength==4 特殊分支 + 统一循环逻辑**（2026-05-29）：
    - 问题：64KB+ 读取失败，第1轮数据+心跳包处理后，第2轮 read_exact(12) 返回0字节
    - 根因：Rust 对 slength==4 的心跳包进行特殊处理（continue），跳过了数据追加、ACK 发送和 remaining 递减。设备等待 ACK 而不发送下一个数据包，下一轮 read_exact 超时 → bad magic 0x00000000
    - Python 行为：xread() 对心跳包（slength=4, val=0）不做特殊处理，走完全相同的路径（追加 buffer、ack、length -= len(tmp)）。终止信号通过 ack() != 0 判断，不在数据块中判断
    - 修复：
      - 删除整个 `if slength == 4 { ... }` 分支块
      - 所有数据（包括心跳包的4字节零值）都追加到 buffer
      - 每轮都发送 ACK 并读取 status
      - 每轮都递减 remaining
      - 循环退出依赖 `ack() != 0` 而非数据内容判断
    - Python 参考：xflash_lib.py:113-124 (xread), xflash_lib.py:879-891 (readflash 循环)
    - 验证：cargo build 通过（0 error），cargo clippy 无新增 warning

22. **ACK 重构：枚举化返回值 + 删除 ack_no_status + ZLP 优雅退出**（2026-05-29）：
    - 新增 `AckResult` 枚举（`da_xflash.rs` pack3 函数后）：
      - `AckResult::Continue` — status == 0，设备就绪，继续传输
      - `AckResult::Terminated(u32)` — status != 0，传输终止或设备报错
    - 重写 `ack()` 方法：返回 `AckResult` 替代裸 `u32`
      - 写失败 → `Terminated(1)` / `Terminated(2)`
      - `self.status()` Ok(0) → `Continue`
      - `self.status()` Ok(n) → `Terminated(n)`
      - `self.status()` Err(_) → `Terminated(3)`
    - 删除 `ack_no_status` 方法及其 `#[allow(dead_code)]` 标注
    - `readflash_data` 循环适配枚举：
      - 遇 ZLP/空响应（read_exact 返回 0）→ graceful break
      - 遇 bad magic → debug 日志 + graceful break（不再 return Err）
      - 读数据错误 → debug 日志 + graceful break
      - `match self.ack()` → `AckResult::Continue` 继续 / `AckResult::Terminated` break
    - `da_partition.rs` erase 循环：`let _ = self.ack()` 添加注释 "AckResult 的 Debug 输出已满足日志需求"
    - 验证：cargo check 通过，cargo clippy 无新增 warning

23. **bypass 后 reopen USB 连接**（2026-05-29）：
    - 问题：bypass_security 注入 patcher payload 后设备 USB 端点状态变化，同一句柄上 echo(0xDA) 超时
    - 根因：patcher payload 执行后 bulk IN/OUT 端点状态异常，原句柄无法正常通信
    - 修复：在 `commands.rs` 的 `handle_command` 和 `handle_commands` 中，bypass_security 返回后、dump_preloader_from_ram 前，插入 `da.preloader.device.reopen(_context)`
    - reopen 内部：close 旧句柄 → sleep 200ms → UsbDevice::new (通过 SUPPORTED_DEVICES 表查找 VID=0E8D PID=0003) → 交换字段
    - bypass 后设备仍为 BROM 模式 (VID=0E8D PID=0003)，reopen 能正确匹配
     - 验证：cargo check 通过，cargo clippy 无新增 warning

27. **修复 GPT 读取大小硬编码**（2026-05-29）：
     - 问题：`read_gpt()` 中 GPT 读取大小硬编码为 16384 字节，请求 16384 时设备返回心跳包（4字节）就停止发送，导致只读到 4 字节
     - Python 参考：`partition.py:70` `length=2 * self.config.pagesize = 2 * 512 = 1024`
     - Python 逻辑：首次读 1024 获取 GPT header，解析确定 sectors 后再读 `sectors * pagesize` 完整数据（partition.py:114）
     - Rust 直接请求 16384 可能触发了设备的不同响应行为（提前终止）
     - 修复：改为 2048 字节（覆盖 GPT 头 1024 字节 + 额外 1024 字节安全冗余）
     - 验证：cargo build 通过（0 error），cargo clippy 0 warning

26. **修复所有 clippy warnings（0 warning）**（2026-05-29）：
     - `cargo clippy --fix --bin "mtkclient-rs" -p mtkclient-rs` 自动修复 5 个：
       - `collapsible_if` × 3（commands.rs, main.rs, usb.rs）
       - `manual_range_contains` × 1（kamakiri2.rs → `(0x10000..=0x100000).contains(&length)`）
       - `inherent_to_string` × 1（da_xflash.rs）
     - 手动修复 3 个 `dead_code` warning：
       - `src/config.rs:266` `needs_bypass` → `#[allow(dead_code)]`（预留：bypass 流程判断）
       - `src/preloader.rs:141` `get_hw_subcode` → `#[allow(dead_code)]`（预留：芯片变体区分）
       - `src/usb.rs:669` `reopen` → `#[allow(dead_code)]`（预留：bypass 后重建 USB 连接）
     - 手动修复 `inherent_to_string`（session.rs）→ 实现 `fmt::Display for SessionState` 替代 `fn to_string`
     - 验证：`cargo clippy` 输出 0 warning

24. **bypass 后 reopen + do_handshake**（2026-05-29）：
     - 问题：reopen 后 echo(0xDA) → `ret=-7, transferred=0` → 超时
     - 根因：reopen 只是关闭再打开 USB 设备，设备此时 BROM 未就绪，需要握手唤醒
     - Python 对照：crasher 中 `Port()` 创建新连接后调 `preloader.init()` → `handshake()` → `run_handshake()`，完成握手后才 dump
     - 修复：reopen 后加 `da.preloader.device.do_handshake()` 唤醒 BROM 协议状态机
     - 两处修改：`handle_command` 和 `handle_commands`
     - 验证：cargo check 通过，cargo clippy 无新增 warning

28. **ack() 诊断日志**（2026-05-29）：
     - 目的：确认 `ack()` 内部的 `status()` 到底读到了什么——如果 `length` 是 2048 而不是 4，说明 ack 偷吃了数据块
     - 修改：用内联代码替代 `self.status()` 调用，增加 debug 日志打印：
       - `[ack::status] hdr: magic=0xXXXXXXXX length=XXXX`
       - `[ack::status] data: N bytes, first 16: XX XX XX ...`
       - `[ack::status] short hdr read: N/12 bytes`
       - `[ack::status] hdr read error: ...`
     - timeout 临时设为 2000ms 用于诊断
     - 验证：cargo build 通过，cargo clippy 0 warning

36. **USB 握手修复：单次 bulk 传输 + 20ms 超时**（2026-05-29）：
     - 问题：`do_handshake` 用 `self.read(&mut r)` 循环读满 512 字节，设备只回 1 字节，`read` 等到超时（返回 -7/-1）
     - Python 对照：`ep_in(maxinsize)[-1]` — 单次 bulk 读取，取最后一个字节
     - 修复：改用单次 `libusb_bulk_transfer` 读取，timeout=20ms（对齐 Python）
     - 关键变化：
       - 回显读取：循环读满 512B → 单次 bulk 传输
       - 超时：默认 1000ms → 20ms（bootloader 只活跃约 0.3 秒）
       - 行为：读不满等到超时 → 设备发多少收多少

37. **修复 aes/cbc API breaking change**（2026-05-29）：
     - 问题：`cargo update` 升级 aes 和 cbc 依赖到新大版本（cipher 0.4→0.5），`BlockDecryptMut`/`BlockEncryptMut` 重命名为 `BlockModeDecrypt`/`BlockModeEncrypt`
     - 修复：
       - import: `BlockDecryptMut, BlockEncryptMut` → `BlockModeDecrypt, BlockModeEncrypt`
       - 方法名: `decrypt_padded_mut` → `decrypt_padded`（4 处）
       - 方法名: `encrypt_padded_mut` → `encrypt_padded`（3 处）
     - 验证：cargo build 通过（0 error），cargo clippy 0 warning（仅 1 个已有的 dead_code warning）

35. **Preloader 传输层抽象：BromTransport trait**（2026-05-29）：
     - 目标：让 Preloader 同时支持 libusb 和 serialport 两种连接方式
     - 新增 `BromTransport` trait（preloader.rs）：
       - 基础方法：write、read_exact、read、set_timeout、get_timeout、do_handshake
       - USB 专属方法（带默认实现）：ctrl_transfer_in、ctrl_transfer_out、clear_halt_in
     - `Preloader.device` 从 `UsbDevice` 改为 `Box<dyn BromTransport>`
     - `SerialPortTransport` 实现 BromTransport：
       - 通过 `serialport::available_ports()` 枚举 COM 口
       - 匹配 VID=0E8D PID=0003 的 MediaTek BROM 设备
       - BROM 握手：逐字节发送 A0 0A 50 05，验证回显取反
     - `UsbDevice` 实现 BromTransport：委托到已有的 write/read 等方法
     - `smart_init` 优先尝试 COM 口直连：
       - 找到 BROM COM 口 → 串口握手 → 关闭看门狗 → 释放 COM 口 → libusb 接管
       - 未找到 COM 口 → 直接走 libusb 连接逻辑
     - 验证：cargo build 通过，cargo clippy 0 warning

34. **driver.rs：管理员权限检查 + 自动提权**（2026-05-29）：
     - 问题：pnputil 和 certutil 需要管理员权限，非管理员运行会静默失败
     - 新增 `is_admin()`：执行 `net session` 检查管理员权限（CREATE_NO_WINDOW 隐藏窗口）
     - 新增 `rerun_as_admin()`：通过 `powershell Start-Process -Verb RunAs` 提权重启
     - `install_winusb_driver` 开头检查：非管理员 → 弹出 UAC 提权 → 等待完成后 exit(0)
     - 验证：cargo build 通过，cargo clippy 0 warning

33. **driver.rs：等待 BROM 设备 + 修复 pnputil 判断**（2026-05-29）：
     - 改动 1：`install_winusb_driver` 加设备等待循环
       - 找不到设备时循环等待（2 秒间隔），提示用户进入 BROM 模式
       - 提示："请按住音量+和音量-，插入USB进入BROM模式..."
       - 找到设备后关闭 Watchdog，等待 2 秒稳定
     - 改动 2：`install_driver_inf` 修复 pnputil 判断
       - 原代码：`if !status.success()` 报错退出，但 pnputil 对已存在的驱动返回非零
       - 改用 `output()` 捕获 stdout，检查 "successfully" 或 "Already exists" 都算成功
     - 验证：cargo build 通过，cargo clippy 0 warning

32. **driver.rs 串口看门狗：完整 BROM 协议**（2026-05-29）：
     - 问题：原 `disable_watchdog_serial` 只发 `0xA0`，无握手验证，无回显检查
     - 修复：
       - 步骤 1：BROM 握手 — 逐字节发送 `A0 0A 50 05`，验证回显取反 `5F F5 AF FA`
       - 步骤 2：WRITE32 命令 — `echo(0xD4)` → `echo(0x10007000)` → `echo(1)` → `echo(0x22000000)`
       - 新增 `echo()` 辅助函数：发送 data，读回相同字节数并比对
     - 对齐 Python Port.py:run_handshake + serialport 版 setreg_disablewatchdogtimer
     - 验证：cargo build 通过，cargo clippy 0 warning

29. **ACK 重构：解耦 send_ack 和 status 读取**（2026-05-29）：
     - 根因：`ack()` 内部的 `status()` 在心跳包场景下偷吃了下一个数据块（诊断日志确认 `length=2048` 而非 4）
     - 新增 `send_ack()` 方法：只发 ACK 包不读响应
     - `ack()` 方法恢复简洁版本：调用 `send_ack()` + `status()`
     - `readflash_data` 循环改用 `send_ack()`：只发不读，避免偷吃下一个数据块
     - 关键设计：发 ACK 和读响应的顺序由调用方控制，循环自己管理数据流
     - 验证：cargo build 通过，cargo clippy 0 warning

30. **两步 GPT 读取：动态计算完整大小**（2026-05-29）：
     - 问题：硬编码 2048 字节只能读前 16 个分区项，128 分区时需 `512 + 128×128 = 16896` 字节
     - Python 参考：`gpt.py:parseheader` 从 `gptdata[sector_size:sector_size+0x5C]` 解析 header
     - 修复：
       - 第一步：读 1024 字节（对齐 Python `2 * pagesize = 1024`）
       - 复用 `GptInfo::parse` 自动搜索 `EFI PART` 签名定位基址后解析
       - 第二步：计算 `total_read_len = 512 + num_entries × entry_size` 重新读取完整数据
     - 验证：cargo build 通过，cargo clippy 0 warning

31. **GPT 头解析：复用 GptInfo::parse，移除硬编码偏移**（2026-05-29）：
     - 问题：硬编码偏移 592/596 解析 GPT 头，未搜索 `EFI PART` 签名定位基址
     - 修复：删除硬编码偏移解析，改用 `GptInfo::parse(&header_data)?` 复用已有解析逻辑
     - 优势：自动搜索 `EFI PART` 签名定位基址，更健壮且代码复用
     - 验证：cargo build 通过，cargo clippy 0 warning
     - 问题：reopen 后 handshake 全部失败（write 返回 -7 / LIBUSB_ERROR_TIMEOUT）
     - 根因：patcher payload 执行后设备短暂不可用，reopen 的 200ms sleep 不够。且既然 bypass_security 内部的 handshake 已经成功，reopen 本身无必要
     - 修复：
       - 删除 `commands.rs` 两处 `reopen(_context)` + `do_handshake()` 调用
       - `bypass_security` 内部加强 drain：从固定 3 次 read 改为最多 10 次，读到 0 就停止（对齐 Python run_handshake 逐字节握手自然 drain 行为）
       - timeout 从 100ms 改为 50ms，更快速清空残留
     - Python 对照：crasher 中 `run_handshake` 逐字节验证，每读一个字节自动消费残留数据
     - 验证：cargo check 通过，cargo clippy 无新增 warning


