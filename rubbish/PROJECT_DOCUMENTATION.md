# MTKClient Rust 移植项目 - 终极总结

## 项目架构

```
src/
├── main.rs          # 命令入口，设备初始化流程
├── usb.rs           # USB 通信层，握手，重连
├── preloader.rs     # Preloader 协议，模式检测，漏洞执行
├── da_xflash.rs     # DA 加载，GPT 读取，分区操作
└── config.rs        # MT6768 配置
```

## 完整命令列表

| 命令 | 功能 | Python 对应 | 状态 |
|------|------|-----------|------|
| `printgpt` | 显示GPT分区表 | `get_gpt()` | ✅ 已实现 |
| `r` / `read` | 读取分区 | `readflash()` | ⚠️ 待实现 |
| `w` / `write` | 写入分区 | `writeflash()` | ⚠️ 待实现 |
| `e` / `erase` | 擦除分区 | `formatflash()` | ⚠️ 待实现 |
| `unlock` | 解锁Bootloader | `seccfg unlock` | ⚠️ 待实现 |
| `reset` | 重启设备 | `shutdown()` | ⚠️ 待实现 |

## DA Stage2 协议文档

```
启动握手: 设备发送 0xB1B2B3B4
命令格式: 主机发送 0xF00DD00D + 命令码 + [参数]
命令列表:
  0x6000 - 初始化eMMC → 设备返回 0xD1D1D1D1
  0x1000 - 读块 (block号) → 512字节
  0x1001 - 写块 (block号 + 512字节) → 0xD0D0D0D0
  0x3000 - 重启
  0x4000 - 写内存 (地址 + 大小 + 数据)
  0x4002 - 读内存 (地址 + 大小)
```

## 已知问题清单

| 问题 | 现象 | 位置 | 状态 |
|------|------|------|------|
| BROM→Preloader 切换后命令失败 | 回显 0x52 后超时 | `preloader.rs` | ⚠️ 已修复 |
| DA2 上传后超时 | boot_to 内读取超时 | `da_xflash.rs` | ⚠️ 待确认 |
| BROM 冷启动 Kamakiri 失败 | 读取超时 | `preloader.rs` | ⚠️ 需热启动 |

## 与 Python 2.0.1 的差异对比

| 项目 | Python 实现 | Rust 实现 | 是否一致 |
|------|-----------|------------|---------|
| 握手 | A0 0A 50 05 | A0 0A 50 05 | ✅ |
| 模式检测 | GET_BL_VER | get_blver() | ✅ |
| Kamakiri2 | ctrl_transfer | write() | ⚠️ 简化 |
| DA1 上传 | SEND_DA + JUMP_DA | send_da + jump_da | ✅ |
| DA2 上传 | boot_to | boot_to | ✅ |
| GPT 读取 | 0x1000 命令 | 0x1000 命令 | ✅ |

## 核心功能实现

### 1. USB 通信层 (`usb.rs`)
- 设备检测与握手
- 端口管理与重连
- 数据读写操作
- 支持 BROM 和 Preloader 模式

### 2. Preloader 协议 (`preloader.rs`)
- 模式检测 (BROM vs Preloader)
- Kamakiri2 漏洞执行
- DA 发送与跳转
- Preloader 模式下的通信优化

### 3. DA 加载与 XFlash (`da_xflash.rs`)
- DA 分区解析
- Stage1 和 Stage2 上传
- GPT 分区表读取
- 内存操作命令

### 4. 主命令流程 (`main.rs`)
- 设备初始化
- 命令解析与执行
- 错误处理与重试

## 技术难点与解决方案

### 1. 设备模式切换
- **问题**: BROM 模式执行 Kamakiri 后设备切换到 Preloader 模式，端口可能改变
- **解决方案**: 重连时重新扫描设备，自动检测新端口和模式

### 2. Preloader 模式通信
- **问题**: Preloader 模式下通信协议与 BROM 不同，回显机制差异
- **解决方案**: 在 Preloader 模式下跳过回显检查，直接发送命令

### 3. DA 加载流程
- **问题**: DA 分区解析和上传失败
- **解决方案**: 参考 Python 实现，优化 DA 头解析和数据上传逻辑

### 4. GPT 读取
- **问题**: 不同设备的 GPT 格式差异
- **解决方案**: 支持标准 GPT 和 BPI 格式，自动检测和解析

## 项目状态

- **编译**: ✅ 0 错误，8 个警告（未使用变量）
- **功能**: printgpt 命令已实现，其他命令待开发
- **测试**: 已成功执行 Kamakiri 漏洞，设备切换到 Preloader 模式
- **兼容性**: 支持 MT6768 设备，其他 MTK 设备需测试

## 下一步计划

1. 完善剩余命令实现（read、write、erase、unlock、reset）
2. 优化 Preloader 模式下的通信稳定性
3. 增加更多设备型号支持
4. 完善错误处理和日志系统
5. 添加更多测试用例

## 参考资料

- Python 2.0.1 源码: `D:\test\ZybClient\mtkclient-2.0.1`
- DA Stage2 源码: `stage2.c`
- XFlash 库: `xflash_lib.py`
- MTK 协议文档: 基于 USB 数据包分析