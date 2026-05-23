## MTKClient Rust 项目 - 快速上手

### 项目状态
- 编译: ✅ 0错误 8警告（未使用变量）
- 功能: GPT读写、分区操作、解锁、重启 已对齐Python 2.0.1
- 已知问题: BROM→Preloader切换后命令发送（已修复）

### 关键文件
- `da_xflash.rs`: DA加载核心，Stage2通信
- `preloader.rs`: Preloader协议，漏洞执行
- `main.rs`: 命令入口，模式判断
- `usb.rs`: USB通信层，设备检测与重连

### 协议参考
- `stage2.c`: DA Stage2 源码
- `xflash_lib.py`: Python版 DA加载器
- `mtk_da_handler.py`: DA配置和处理流程

### 核心功能
1. **设备检测与握手**
   - BROM 模式: 发送 A0 0A 50 05 握手
   - Preloader 模式: 直接连接，跳过复杂握手

2. **Kamakiri2 漏洞执行**
   - 适用于 BROM 模式
   - 发送 payload 数据，设备切换到 Preloader 模式

3. **DA 加载流程**
   - 上传 Stage1 到 0x200000
   - 跳转到 Stage1 执行
   - 上传 Stage2 到 DRAM

4. **GPT 读取**
   - 支持标准 GPT 和 BPI 格式
   - 读取分区表和分区信息

### 已修复的问题
- **端口重连**: 设备模式切换后自动检测新端口
- **Preloader 通信**: 跳过回显检查，直接发送命令
- **Kamakiri 执行**: 移除不必要的读取操作，避免超时

### 待解决的问题
- **DA2 上传后超时**: boot_to 内读取超时
- **BROM 冷启动**: 需要热启动才能执行 Kamakiri
- **命令实现**: 完善 read、write、erase、unlock、reset 命令

### 技术栈
- Rust 语言
- serialport 库（USB 通信）
- log 库（日志系统）
- colored 库（彩色输出）

### 运行方式
```bash
# 显示 GPT 分区表
cargo run -- printgpt

# 带详细日志
cargo run -- -vv printgpt
```

### 项目路径
- Rust 项目: `D:\test\ZybClient`
- Python 参考: `D:\test\ZybClient\mtkclient-2.0.1`

---

## 2026-05-08 16:30 CST 更新

### 新增功能 dump_preloader_payload
`Preloader` 结构体新增 `dump_preloader_payload()` 方法，对齐 Python `pltools.py:run_dump_preloader` 流程：
1. `da_write` 发送 `generic_preloader_dump_payload.bin`（592 字节）到 `0x100A00`
2. 写 `ptr_send` 指向 payload 地址
3. `usbread(4)` 读 ack `0xC1C2C3C4`（大端）
4. `usbread(4)` 读长度（小端）
5. `usbread(length)` 按 512 字节分块读取完整 preloader 数据
6. 搜 `MTK_BLOADER_INFO` 提取文件名并保存
7. 返回 `Vec<u8>` 完整数据

`dump-preloader` 命令现已改为调用此方法（`main.rs` 和 `commands.rs` 均已更新）。

### 保留功能 dump_preloader_from_ram
`dump_preloader_from_ram()` 仍然保留在 `preloader.rs` 中：
- 使用 `read32_brom` echo 协议从 RAM 地址 `0x200000` 读取 preloader
- 搜索签名 `\x4D\x4D\x4D\x01\x38\x00\x00\x00`
- 提取长度 → 分块读取 → 搜 `MTK_BLOADER_INFO` → 保存
- 在 main.rs 的 fallback 流程中调用（未指定 --preloader 且非 BROM 模式时）

### Kamakiri2 run_kamakiri2
- 使用 `mt6768_payload.bin`（612 字节）
- 通过 `da_write`/`da_read` 机制发送到 `0x100A00`
- 读 ack `0xA1A2A3A4`（大端）
- 在 `init()` 中调用

### 当前文件结构
```
src/
  main.rs          - 入口，CLI 解析，smart_init，命令分发
  usb.rs           - libusb 封装（UsbDevice/UsbContext），bulk/ctrl transfer，handshake
  preloader.rs     - Preloader 结构体：init/handshake/hw_code/read32/send_da/jump_da/run_kamakiri2/dump_preloader_payload/dump_preloader_from_ram
  da_xflash.rs     - DAXFlash 结构体：DA 解析/patch/upload_da1/upload_da2/boot_to/XFlash 协议/DA extensions
  commands.rs      - 命令处理：handle_command/printgpt/read/write/erase/vbmeta/unlock/dump_preloader
  config.rs        - 配置文件/AppConfig
  cli.rs           - Clap CLI 定义
  driver.rs        - WinUSB 驱动安装/检测
mtkclient-2.0.1/   - Python mtkclient 参考实现
MTK_DA_V5.bin      - MTK AllInOne DA 文件
mt6768_payload.bin       - Kamakiri2 安全绕过 payload（612 字节）
generic_preloader_dump_payload.bin - Preloader dump payload（592 字节）
preloader_k69v1_64_k419.bin - 已知 preloader 文件
```

### 关键常量
| 项目 | 值 |
|------|-----|
| USB VID/PID (BROM) | 0x0E8D / 0x0003 |
| USB VID/PID (Preloader) | 0x0E8D / 0x2000 |
| Kamakiri2 ack | 0xA1A2A3A4 (大端) |
| Dump preloader ack | 0xC1C2C3C4 (大端) |
| Kamakiri2 payload addr | 0x100A00 |
| ptr_send base | 0xC190 |
| ptr_da_bra | 0xC650 |
| ptr_da | 0xC598 |
| watchdog addr | 0x10007000 |
| XFlash magic | 0xFEEEEEEF |
| SYNC_SIGNAL | 0x434E5953 |
| Preloader RAM addr | 0x200000 |
| DA extensions addr | 0x4FFF0000 |
| DA2 addr | 0x80000000 |

### 协议对齐状态
| 功能 | Python 参考 | Rust 状态 |
|------|------------|----------|
| USB handshake | usb.py handle_handshake | 已对齐 preloader.rs |
| hw_code 读取 | brom_register_access | 已对齐 |
| Kamakiri2 bypass | kamakiri2.py runpayload | 已对齐 run_kamakiri2 |
| DA 解析/patch | xflash.py parse_da/patch_da | 已对齐 da_xflash.rs |
| DA1 upload | xflash.py upload_da1 | 已对齐 upload_da1 |
| DA2 upload | xflash.py boot_to | 已对齐 boot_to |
| DA extensions | xflash.py patch + boot_to | 已对齐 generate_da_extensions |
| Preloader dump (payload) | pltools.py run_dump_preloader | 已对齐 dump_preloader_payload |
| Preloader dump (RAM) | mtk_da_handler.py dump_preloader_ram | 已对齐 dump_preloader_from_ram |
| GPT read | xflash_lib.py read_gpt | 已对齐 read_gpt |
| EMI extract | xflash.py m_extract_emi | 已对齐 extract_emi |

### 待实现
- read_partition（读取指定分区到文件）
- write_partition（写入文件到指定分区）
- erase_partition（擦除分区）
- vbmeta patch（修补 vbmeta）
- unlock_bootloader（解锁 bootloader）