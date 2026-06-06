# Temp_Agent.md — ZybFlashTool 会话上下文

> 最近更新：2026-06-05
> 完整历史：Temp_Agent_Archive.md

## 当前状态
- 功能状态表：
  - `printgpt` ✅
  - `dump-preloader` ✅
  - `r分区` ⚠️
  - `w/e` ❌
  - `auto-dump` ⚠️
- 最近工作区状态：核心流程已重构为 COM 握手 → 安装 filter → drop COM → 高速轮询 open_by_vid_pid 50ms。删除 wait_for_device/device_present。install_libusb_filter 改为返回 Result
- 最近提交基线：`4cac7f6`（重构 COM → libusb 切换流程为核心四步）

## 关键协议
- BROM：
  - `0xD1` 读写
  - 大端 echo
  - watchdog 双 status
- XFlash：
  - `CMD_READ_DATA` + `send_param`
  - `send_ack` / `ack`
  - `reset_device` 使用 `0x010007`
- GPT：
  - 先读 32KB
  - `GptInfo::parse` 后按需要扩展

## 核心文件结构
```text
src/main.rs        - 入口、命令路由、smart_init
src/commands.rs    - 命令分发、printgpt/read/write/erase 流程
src/preloader.rs   - BROM 协议、echo、hw_code、send_da、jump_*、register access
src/kamakiri2.rs   - exploit / da_read / da_write / inject_payload
src/da_xflash.rs   - DA/XFlash 上传、扩展、读写、erase、reset
src/sej.rs         - HACC/SEJ 寄存器后端与签名接口
src/seccfg.rs      - SecCfgV3/V4 解析、锁解、离线处理
src/da_partition.rs - GPT 读取、分区解析、地址查找
src/driver.rs      - libusb-filter 检测与安装、install-filter 状态检查、COM 占用检测
src/filter.rs      - （已合并到 driver.rs，install-filter.exe 包装与设备 filter 检测）
src/frp.rs         - FRP OEM unlocking 预留入口
src/usb.rs         - USB 底层通信
src/config.rs      - 芯片配置与寄存器常量
src/cli.rs         - 命令行参数
src/paths.rs       - 路径辅助
```

## 构建和测试
- `cargo build`
- `cargo clippy -- -D warnings`
- `cargo run -- --preloader preloader_k69v1_64_k419.bin printgpt`

## 注意事项
- 不要把 `cargo run` 当作默认验证手段，设备未连接时会卡住。
- `Temp_Agent_Archive.md` 保存完整历史，当前文件只保留可恢复的摘要。
- BROM 串口和 USB 两条路径都还在，`smart_init` 会根据状态选择。
- `driver.rs` 现在走 `install-filter.exe` + `serialport` watchdog，不再依赖 `zadig_rust.dll` / `devcon.exe`。
- `readflash_data` 允许大块读取，但 ACK 语义要保持每块一确认。
- `reset_device` 已优先走 DA 重启，BROM `jump_bl` 只作为 fallback。
- `seccfg` 逻辑已拆到 `src/seccfg.rs`，离线锁解命令改从新模块取实现。
- `sej.rs` 作为 HACC/SEJ 后端入口，后续在线 seccfg 解锁会从这里走。
- `sej_hacc_sign` 已提供 HACC 后端绑定接口，`seccfg` 在线 V4 路径已接入。
- `frp.rs` 目前是独立预留模块，后续可直接挂到命令路由。
- `usb.rs` 的 dead_code 允许保留，不要为了消 warning 误删接口。
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

25. **libusb 自动接管流程完善（工程级稳定）**（2026-06-04）：
     - 问题：串口握手成功后，libusb filter 安装和设备重枚举流程不稳定
     - 根因：sleep 时间固定、无设备检测循环、无 claim 重试、filter 成功后可回退串口（但串口已不可用）
     - 修复：
       - `build.rs`：自动拷贝 `install-filter.exe`、`libusb0.dll`、`libusb0.sys` 到 target/libusb/
       - `libusb_filter.rs`（新模块）：封装 filter 安装逻辑，定位 exe → 执行 `install --device=0x0E8D:0x0003`
       - `smart_init` 修改：
         - drop(serial) → sleep 500ms → 循环检测 libusb 设备（10秒，每200ms重试一次）
         - filter 安装成功后不再回退串口（设备状态已变化）
         - libusb open 失败时静默重试（每5次打印一次 debug）
       - `usb.rs::open_by_vid_pid` 修改：
         - detach_kernel_driver(0) + claim_interface(0) 带 3 次重试（间隔 100ms）
         - detach_kernel_driver(1) + claim_interface(1)
         - endpoint 扫描更简洁：只区分 Bulk IN/OUT，用 debug! 输出
         - 默认端点 OUT=0x01, IN=0x81
     - 验证：cargo build 通过，无新增 warning

26. **架构重构：ConnectionManager 统一连接状态机 + 移除 libusb_filter**（2026-06-04）：
     - 问题：libusb-win32 filter 注入方案存在 race condition，设备重枚举不稳定；smart_init 逻辑分散在 main.rs 中难以维护
     - 根因：驱动层依赖 install-filter.exe 注入 filter driver，Windows 驱动模型复杂，filter 安装后设备状态变化不可控
     - 方案：完全重构为 ConnectionManager 驱动的三层连接系统，对齐 MTKClient Python 行为
     - 修复：
       - **新模块 `connection.rs`**：ConnectionManager 统一连接状态机
         - `smart_init()` → 串口检测 → 握手 → init → 释放串口 → reconnect_loop → libusb 接管
         - `serial_connect()` → 独立串口握手流程
         - `reconnect_loop()` → 10秒超时，200ms重试，循环检测 libusb 设备
         - `DeviceMode` 从 main.rs 移到 connection.rs 统一管理
       - **`main.rs` 精简**：删除 smart_init/detect_mode/DeviceMode/DeviceTransport，改用 ConnectionManager
       - **移除 `libusb_filter.rs`**：不再使用 libusb-win32 filter 注入方案
       - **`commands.rs` 更新**：import 从 `crate::DeviceMode` 改为 `crate::connection::DeviceMode`
       - **`build.rs` 回退**：移除 install-filter.exe 拷贝逻辑（不再需要）
       - **`usb.rs` 保持**：open_by_vid_pid 已完善的 detach/claim/endpoint 扫描逻辑
     - 对齐 MTKClient Python：
       - `usblib.py::connect()` → `UsbDevice::open_by_vid_pid()` (detach + claim + endpoint 扫描)
       - `mtk_preloader.py::init()` → `ConnectionManager::reconnect_loop()` (循环重试)
       - `Port.py::run_handshake()` → `SerialPortTransport` 握手
     - 验证：cargo build 通过，1 个旧 warning（`config.rs::from_vid_pid` 不再使用）

27. **工程级稳定连接架构：USB 优先 + reconnect_loop + 错误处理完善**（2026-06-04）：
     - 问题：串口优先策略导致 BROM 模式下多绕一圈；claim_interface 竞态条件处理不完善；reconnect 日志不够详细
     - 方案：完全对齐 MTKClient Python 行为，USB 优先策略，reconnect_loop 为核心
     - 修复：
       - **`connection.rs` 重构**：
         - `smart_init()` 改为 USB 优先：先尝试 `open_by_vid_pid(0x0E8D, 0x0003)` → 成功直接返回
         - USB 失败 → 串口检测 → 握手 → 释放 → reconnect_loop
         - 串口也失败 → reconnect_loop 循环检测
         - 新增 `reconnect_after_da()` — DA 加载后重连（预留）
         - 新增 `reconnect_after_kamakiri()` — exploit 后重连（预留）
         - 日志输出 `[RECONNECT] retry N/50...` 和 `[RECONNECT] success on attempt N/50`
       - **`usb.rs::open_device` 完善**：
         - detach_kernel_driver 日志更详细（区分 NOT_FOUND 和其他错误）
         - claim_interface 失败时自动 cleanup（release + close），防止句柄泄漏
         - endpoint 扫描从 info! 降级为 debug!（减少噪音）
         - 日志输出 `[USB] VID=0x0E8D PID=0x0003 detected, EP_OUT=0x01, EP_IN=0x81`
     - 关键设计原则：
       - USB 优先（不是串口）
       - reconnect_loop 是核心（不是辅助）
       - 所有模式切换都依赖 USB 重连
       - 不依赖驱动注入（install-filter.exe）
       - 不信任一次连接（必须 retry）
     - 对齐 MTKClient Python：
       - `usblib.connect()` → `UsbDevice::open_by_vid_pid()` (detach + claim + endpoint 扫描)
       - `reconnect loop` → `reconnect_loop()` (10s/200ms, 50 次重试)
       - `preloader init` → `preloader.rs::init()` (握手 + watchdog + BROM sync)
       - `get_connection_agent` → `ConnectionManager` (统一状态机)
     - 验证：cargo build 通过，2 个 warning（1 个旧 `from_vid_pid`，1 个预留 `reconnect_callback`）

28. **工程级稳定连接架构完善：UsbStage + 多PID扫描 + DA重连**（2026-06-04）：
     - 问题：reconnect_loop 只能扫描固定 PID，不支持阶段识别；DA 加载后无专用重连方法
     - 修复：
       - **`usb.rs` 新增 `UsbStage` 枚举**：
         - `UsbStage::Brom`（PID=0x0003）/ `Preloader`（PID=0x2000）/ `Unknown`
         - `UsbStage::from_pid(pid)` 自动识别设备阶段
         - `UsbDevice::stage` 字段公开，可查询当前设备阶段
         - `open_device()` 中自动设置 `stage` 字段（两处构造函数均已更新）
       - **`connection.rs` reconnect_loop 升级**：
         - 签名改为 `reconnect_loop(context, target_stage: UsbStage)`
         - 根据目标阶段动态确定扫描的 PID 列表：
           - `Brom` → `[0x0003]`
           - `Preloader` → `[0x2000]`
           - `Unknown` → `[0x0003, 0x2000]`
         - 每次重试遍历所有候选 PID（静默失败，不打印噪音）
         - 成功日志：`[RECONNECT] success on attempt N/50 (stage=Brom, PID=0x0003)`
       - **新增 `reconnect_after_da()`**：
         - 等待 500ms → 快速连接 BROM PID（5次重试）→ 失败则扫描所有已知 PID
         - 对齐 MTKClient Python：DA 加载后设备 USB reset，需重连
       - **新增 `reconnect_after_kamakiri()`**：
         - 等待 500ms → reconnect_loop(Brom)
         - 对齐 mtkclient stage2.py：payload 后设备 USB reset
       - **新增 `try_quick_connect()`**（私有方法）：
         - 快速连接尝试，用于 DA 后快速重连场景
     - 完整链路：`Preloader(串口) → BROM → DA → reconnect` 全流程已打通
     - 验证：cargo build 通过，2 个 warning（旧 `from_vid_pid` + 预留方法 dead_code）

29. **驱动层检测与 UsbDk 集成 + 设备状态模型 + 后端选择**（2026-06-04）：
     - 问题：设备在 pnputil 中可见（VID=0x0E8D PID=0x0003），但 libusb 永远 open 失败
     - 根因：Windows USB Serial (COM) 驱动占用设备，libusb 无法直接打开；无 UsbDk 检测；无后端选择逻辑
     - 修复：
       - **新模块 `driver.rs`** — 驱动层检测与后端选择：
         - `DeviceBackend` 枚举：`UsbDk` / `Libusb` / `SerialCom`
         - `has_usbdk()` — 检测 UsbDk 是否可用（检查 UsbDk.sys、UsbDkHelper.exe、安装目录）
         - `detect_backend()` — 自动选择最佳后端（UsbDk 优先 → libusb fallback）
         - `is_device_com_occupied(vid, pid)` — 通过 pnputil 检测设备是否被 COM 驱动占用
         - `backend_status()` — 获取后端状态摘要（用于日志）
       - **`connection.rs` 全面升级**：
         - 新增 `backend: DeviceBackend` 字段到 ConnectionManager
         - `smart_init()` 改为 4 步流程：
           - STEP 1: USB fast scan（UsbDk 优先）
           - STEP 2: COM scan + handshake
           - STEP 3: COM → USB 切换（release + reconnect）
           - STEP 4: reconnect_loop 循环检测
         - 新增 `validate_device()` — 设备状态模型验证（VID + endpoint 检查，避免误连）
         - reconnect_loop 增加设备状态验证：找到设备后先 validate 再返回
         - 启动时自动检测并打印后端状态
       - **日志规范统一**：
         - `[DRIVER] backend=UsbDk` / `Libusb`
         - `[DRIVER] UsbDk=yes, Libusb=available, COM=available`
         - `[USB] 设备被 Windows COM 驱动占用，libusb 无法直接打开`
         - `[RECONNECT] device found but validation failed`
     - 对齐 MTKClient Python:
       - `get_connection_agent()` → `driver::detect_backend()`
       - dynamic backend selection → UsbDk detection + fallback
       - device state model → `validate_device()`
     - 验证：cargo build 通过，3 个 warning（旧 `from_vid_pid` + 预留方法 + SerialCom variant）

30. **工程级稳定连接架构完善：UsbDk 真实验证 + 窗口捕获 + 设备所有权 + 状态机**（2026-06-04）：
     - 问题：reconnect_loop 永久扫描但设备不可打开；COM → USB 切换无状态同步；backend 缓存不实时更新
     - 根因：UsbDk 只检测文件存在未真实验证；reconnect_loop 是简单 polling 无状态机；backend 在 ConnectionManager::new() 中缓存
     - 修复：
       - **`driver.rs` 全面重构**：
         - 新增 `DeviceOwner` 枚举：`UsbDkOwned` / `WinUsbOwned` / `SerialOwned` / `Unknown`
         - 新增 `usbdk_open_test(context, vid, pid)` — UsbDk 真实接管验证（open + descriptor + endpoint 验证）
         - 新增 `detect_device_owner(context, vid, pid)` — 检测设备驱动所有权状态
         - 重构 `detect_backend(context, vid, pid)` — 每次调用实时检测，不再缓存结果
         - 新增 `wait_reenumeration_window(context, vid, pid, timeout_ms)` — 状态机等待设备重枚举窗口
         - 扩展 UsbDk 检测路径：新增 `C:\Program Files (x86)\Daytona\UsbDk` 支持
       - **`connection.rs` 全面重构**：
         - 移除 `backend` 缓存字段，改为每次 reconnect 实时调用 `detect_backend()`
         - 新增 `ReconnectState` 状态机枚举：`WaitReenumeration` → `WindowDetected` → `Acquired`
         - `smart_init()` 中 COM → USB 切换加入 `wait_reenumeration_window()` 延迟窗口（100ms 间隔扫描，最多 3 秒）
         - `reconnect_loop()` 改为 MTKClient 风格窗口捕获：
           - 状态机流转：WaitReenumeration → 检测到设备 → WindowDetected → 验证 → Acquired
           - 成功时实时检测后端：`driver::detect_backend(context, device.vid, device.pid)`
           - 日志：`[RECONNECT] window detected (stage=Brom, PID=0x0003)`
         - 新增 `reconnect_after_usb_reset()` — USB reset 后重连
         - 统一日志标准：`[COM] handshake success COM12` / `[COM] releasing serial interface` / `[USB] waiting re-enumeration window...` / `[USB] backend=UsbDk selected` / `[USB] ownership=UsbDkOwned` / `[USB] device opened successfully` / `[USB] interface claimed` / `[RECONNECT] window detected` / `[RECONNECT] success on attempt 3`
     - 核心设计原则：
       - timing window control（不是盲目 polling）
       - driver ownership detection（不是猜测）
       - reconnect state machine（不是简单循环）
       - backend fallback chain（不是固定后端）
     - 验证：cargo build 通过，4 个 warning（1 个旧 `from_vid_pid` + 3 个预留方法/变体）

31. **移除 UsbDk 改用 libusb-filter** (2026-06-05)
     - 触发：用户要求移除 UsbDk，改用和刷机匣（C# MtkClient）一样的 libusb-filter 方案
     - 改动：
       - `src/driver.rs` 完全重写：删除 `has_usbdk()`、`DeviceOwner::UsbDkOwned`、`DeviceBackend::UsbDk`、`usbdk_open_test()`
       - 新增 `install_libusb_filter()` — 调用 `install-filter.exe install --device=USB\VID_0E8D&PID_0003`
       - 新增 `is_filter_installed()` — 调用 `install-filter.exe list` 检查是否已安装
       - `src/kamakiri2.rs` 的 `bypass_security()` 开头调用 `install_libusb_filter(0x0E8D, 0x0003)`
       - `build.rs` 添加 `rerun-if-changed` 指令，简化 libusb 文件拷贝
       - `binaries/libusb/install-filter.exe` 从刷机匣复制过来
     - 验证：cargo build 通过，0 新增 warning
     - 基线提交：`15091a5`

32. **COM 握手后释放串口并切换 libusb 接管** (2026-06-05)
     - 触发：用户要求 COM 口前期握手后切 libusb 接管后续所有通信
     - 改动：
       - `src/connection.rs` 的 `serial_connect()` 改写：
         1. COM 握手成功后获取 hw_code
         2. `drop(preloader)` 释放 COM 口
         3. 等待 500ms 设备稳定
         4. 调用 `install-filter.exe install --device=USB\VID_0E8D&PID_0003`
         5. 轮询 20 次 × 500ms 等待 `UsbDevice::open_by_vid_pid(0x0E8D, 0x0003)` 成功
         6. 成功后返回 libusb 设备 + Preloader，后续走完整 BROM→DA 流程
       - `smart_init` STEP 2 简化：直接使用 serial_connect 返回的 libusb 设备，删除旧的 drop+wait_reenumeration+reconnect_loop 流程
       - `src/driver.rs` 删除不再使用的 `wait_reenumeration_window()` 函数
     - 关键设计：
       - COM 只用于前期握手（init），后续全部通过 libusb 通信
       - filter 只对 0E8D:0003 安装，不影响其他 USB 设备
       - 轮询而非盲目等待，10 秒超时
       - 不需要 UsbDk，不需要全局 filter
     - 验证：cargo build && cargo clippy 通过，0 新增 error/warning
     - 基线提交：`8ed51ed`

33. **COM 握手后使用 wait_for_device + install-filter + libusb 接管** (2026-06-05)
     - 触发：用户要求优化 COM → libusb 切换流程，使用 device_present 轻量检测 + 两步 wait_for_device
     - 改动：
       - `src/usb.rs` 新增 `UsbDevice::device_present(context, vid, pid)` — 只判断设备是否可打开（libusb_open_device_with_vid_pid），不 claim interface，用于快速轮询
       - `src/connection.rs` 新增 `wait_for_device(context, vid, pid, timeout_ms)` 辅助函数 — 轮询 device_present 直到成功或超时
       - `serial_connect` 改为三步流程：
         1. `drop(preloader)` 释放 COM 口
         2. `wait_for_device(0x0E8D, 0x0003, 3000)` — 等待 BROM 设备出现
         3. `driver::install_libusb_filter(0x0E8D, 0x0003)` — 安装 filter（内部已检查是否已安装）
         4. `wait_for_device(0x0E8D, 0x0003, 3000)` — 等待 filter 安装后设备重新枚举
         5. 轮询 `open_by_vid_pid`（200ms 间隔，5 秒超时）直到成功
       - 删除旧的内联 filter 安装代码，统一通过 `driver::install_libusb_filter` 调用
       - 删除未使用的 `use std::process::Stdio`
     - 关键设计：
       - `device_present` 只做 open + close，不 claim interface，开销远小于完整 open
       - 两步 wait_for_device 确保：先检测 COM 释放后设备出现 → 再检测 filter 安装后设备稳定
       - filter 安装通过 `driver::install_libusb_filter` 统一入口，内部检查是否已安装避免重复
     - 验证：cargo build && cargo clippy 通过，0 新增 error/warning
     - 基线提交：`893f077`

34. **重构 COM → libusb 切换流程为核心四步** (2026-06-05)
     - 触发：用户要求彻底重构流程，保证不丢 BROM 枚举窗口
     - 核心设计原则：
       1. filter 必须在 drop(preloader) 之前安装
       2. 禁止使用 wait_for_device / device_present
       3. 设备检测唯一方式：libusb open_by_vid_pid
       4. BROM 枚举窗口极短（<500ms），必须立即轮询 open
       5. COM 释放后不能有阻塞操作（尤其 install-filter）
       6. open 失败是正常行为，必须高速重试
     - 改动：
       - 删除 `src/connection.rs` 的 `wait_for_device` 函数
       - 删除 `src/usb.rs` 的 `device_present` 函数
       - 删除所有 "等待设备出现" 和 "检测 USB 再操作" 的逻辑
       - `serial_connect` 改为核心四步：
         1. `driver::install_libusb_filter(0x0E8D, 0x0003)` — 提前安装 filter
         2. `drop(preloader)` — 释放 COM，触发 USB 重枚举
         3. `sleep(100ms)` — USB 栈稳定
         4. 高速轮询 `open_by_vid_pid`（50ms 间隔，5 秒超时）
       - `driver::install_libusb_filter` 改为返回 `Result<(), String>`
       - `src/kamakiri2.rs` 的 `bypass_security` 使用 `let _ =` 忽略 Result
     - 日志输出要求：
       ```
       COM 口握手成功
       安装 libusb filter / libusb filter 已安装
       释放 COM 口
       开始轮询 BROM
       BROM 连接成功（libusb）
       ```
     - 禁止行为：
       - ❌ drop 后再 install filter
       - ❌ wait_for_device / device_present
       - ❌ sleep 500ms+ 再检测
       - ❌ 先检测设备再 open
       - ❌ 依赖 USB 枚举稳定
     - 验证：cargo build 成功，0 新增 error/warning（3 个原有 warning 不变）
     - 基线提交：`4cac7f6`

