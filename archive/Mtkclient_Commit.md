# Mtkclient v2.0.1 → v2.1.4.1 差异对比报告

> 生成时间：2025-05-17
> 对比目录：`D:\test\ZybClient\mtkclient-2.0.1\` vs `D:\test\ZybClient\mtkclient-2.1.4.1\`
> 目的：评估 2.1.4.1 版本变更对 Rust 移植的影响

---

## 1. 文件结构变化

### 1.1 新增文件（30 个）

| 序号 | 文件路径 | 说明 |
|:---:|---|---|
| 1 | `mtkclient/Library/gui_utils.py` | 新的 GUI 工具模块（LogBase、progress、structhelper_io） |
| 2 | `mtkclient/Library/mtk_crypto.py` | 加密工具（checksum、IMEI、CSSD、MD1 补丁） |
| 3 | `mtkclient/Library/DA/storage.py` | 存储管理重构（UfsInfo、NorInfo、NandInfo、EmmcInfo、RamInfo） |
| 4 | `mtkclient/Library/DA/xmlflash/` (目录) | xml 目录重命名为 xmlflash，内含 xml_cmd/xml_lib/xml_param/extension/v6 |
| 5 | `mtkclient/Library/Partitions/` (目录) | 分区管理新模块（gpt.py、mbr.py、pmt.py、bpi.py） |
| 6 | `mtkclient/Library/Exploit/carbonara.py` | **新 exploit 引擎**（Carbonara 漏洞利用） |
| 7 | `mtkclient/Library/Exploit/exptools/` (目录) | 架构工具（arm_tools.py、aarch_tools.py、arch.py） |
| 8 | `mtkclient/Library/Exploit/heapbait.py` | Heap Bait exploit 工具 |
| 9 | `mtkclient/Library/realtime.py` | 实时操作支持 |
| 10 | `mtk_api.py` | MTK API 入口 |
| 11 | `mtk_iot_api.py` | IoT 设备 API 入口 |
| 12 | `mtkclient/gui/` (目录) | GUI 界面（connect_info、themes、collapsible_splitter） |
| 13 | `mtkclient/config/devicedb.py` | 设备数据库 |
| 14 | `Tools/hardcoded_partition.py` | 硬编码分区工具 |
| 15 | `Tools/samsung_decode.py` | 三星设备解码工具 |

### 1.2 删除文件（14 个）

| 序号 | 文件路径 | 说明 |
|:---:|---|---|
| 1 | `mtkclient/Library/pmt.py` | → 移到 `Library/Partitions/pmt.py` |
| 2 | `mtkclient/Library/gpt.py` | → 移到 `Library/Partitions/gpt.py` |
| 3 | `mtkclient/Library/DA/xml/` (目录) | → 重命名为 `xmlflash/` |
| 4 | `mtkclient/Library/pltools.py` | 功能分散到 Partitions/ 和其他模块 |
| 5 | `src/stage1/` (整个目录) | 模拟载荷工具已移除 |

### 1.3 重命名/移动

| 原路径 | 新路径 | 类型 |
|---|---|---|
| `Library/DA/xml/` | `Library/DA/xmlflash/` | 目录重命名 |
| `Library/pmt.py` | `Library/Partitions/pmt.py` | 移动 |
| `Library/gpt.py` | `Library/Partitions/gpt.py` | 移动 |
| `StructhelperIo` | `structhelper_io` | 类名重命名（驼峰→蛇形） |

---

## 2. 逐文件差异对比

### 2.1 `Library/DA/xflash/xflash_lib.py` [优先级: 高]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| **`ack()` 协议** | 统一 12B 头 + 4B 数据 | **MT6781 用 16B 单包**，其他芯片分两次 | **高** — 需按芯片差异化处理 |
| `send_data()` 块大小 | 固定 64 字节 | **动态使用 EP_OUT.wMaxPacketSize** | 高 — 需查询 USB 端点最大包大小 |
| `writeflash()` 512 对齐 | 仅文件模式 | **无条件 512 对齐**（含内存数据） | 高 — 对齐逻辑需更新 |
| `writeflash()` 长度限制 | 无 | **`min(length, plength)`** | 中 — 防止超长写入 |
| `readflash()` 读取循环 | 一次性 xread | **分块循环读取（最大 4MB chunks）**，带超时和 magic 校验 | 高 — 大文件读取可靠性提升 |
| `readflash()` ACK | 手动组包 | `self.ack(rstatus=False)` | 中 |
| `readflash()` 异常处理 | 简单 try/finally | **完整 try/except/finally**，含 worker 清理 | 中 |
| `readflash()` 进度条 | `progress.show_progress()` | **新 `progress()` 类** | 低 — 仅 UI |
| `send_emi()` 调试日志 | `[EMI DEBUG]` 输出 | **已移除**，错误提示更明确 | 低 |
| `send_param()` | 无 Anti-rollback 处理 | **0xc0020053 错误直接 sys.exit(1)** | 中 — DA 版本回退检测 |
| `status()/xread()` | 解析 datatype | **忽略 datatype（用 `_`）** | 低 |
| `getstorage()` | 存在 | **已删除**，改用 `daconfig.storage.get_storage()` | **高** — 存储管理重构 |
| `partitiontype_and_size()` | 存在 | **已删除**，逻辑迁移到 storage 对象 | **高** |
| `reinit()` 存储设置 | 直接设置 daconfig 属性 | **通过 storage 对象** + `set_flash_size()` | **高** |
| `upload_da()` Extensions | 无条件尝试 | **仅 patch 模式下** | 中 — 需检查 patch 标志 |
| `upload_da()` Extensions 地址 | 硬编码 0x4FFF0000 | **`self.extensions_address` 配置化** | 低 |
| Carbonara exploit | 无 | **新增调用 `carbonara.patchda1_and_upload_da2()`** | 高 — 新漏洞利用路径 |
| `handle_sla()` | RSA key → dummy auth | **先检查厂商（Lake/Tides/Moon/Motorola）硬编码签名** → RSA key → dummy | **高** — SLA 绕过策略大幅增强 |
| `boot_to()` | 硬编码 0x4FFF0000 判断 | **`self.extensions_address` 配置化** | 低 |
| `formatflash()` | `getstorage()` | **`daconfig.storage.get_storage()`** | 高 |
| patch 状态管理 | `self.patch` | **`self.mtk.daloader.patch`** | 中 — 统一管理 |
| 命名规范 | `self.Cmd`、`self.DataType` | **全小写** `self.cmd`、`self.data_type` | 低 |

**重点关注 — ACK 两段写逻辑对比：**

v2.0.1 的 `ack()` 统一发送：
```python
tmp = pack("<III", Cmd.MAGIC, DataType.DT_PROTOCOL_FLOW, 4)  # 12 字节头
data = pack("<I", 0)                                        # 4 字节数据
usbwrite(tmp)
usbwrite(data)
```

v2.1.4.1 针对 MT6781 改为单包：
```python
if chip.dacode in [0x6781]:
    stmp = pack("<IIII", cmd.MAGIC, data_type.DT_PROTOCOL_FLOW, 4, 0)  # 16 字节单包
    usbwrite(stmp)
else:
    # 与 v2.0.1 相同的两段写
```

**Rust 移植结论：** 当前 Rust 实现 `ack()` 使用两段写（pack3 + write），与 v2.0.1 一致。如需支持 MT6781，需要增加单包变体。

---

### 2.2 `Library/Hardware/seccfg.py` [优先级: 高]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| **dm_verity (offset 0x10)** | **critical_lock_state 处理不变** | **与 v2.0.1 完全一致** | **无** — dm_verity bug 未修复 |
| `__init__()` 参数 | `(_hwc, mtk)` | `(_hwc, mtk, custom_sej_hw=None)` | 中 — 新增自定义 SEJ 参数 |
| **V3 `verify()` 新方法** | 无 | **检测标记在偏移 0 或 0x850** | **高** — 更灵活的验证 |
| V3 `parse()` 验证方式 | 硬编码 `err[:4]` | **`self.verify(err)`** | 高 |
| V3 `create()` 状态检查 | 两条检查 | **新增"已锁定/已解锁"提前返回** | 中 — 更明确的错误信息 |
| V4 `parse()` custom_sej_hw | 无 | **强制 None（死代码）** | 低 — HW/HWXOR 检测未启用 |
| V4 `create()` HW/HWXOR 分支 | 无 | 新增但实际不可达 | 低 |
| `protect()` 方法 | 无 | 新增（被注释掉） | 低 |
| 导入路径 | `Library.utils` | **`Library.gui_utils`** | 中 — 模块重构 |
| `StructhelperIo` | 大写驼峰 | **小写蛇形 `structhelper_io`** | 低 |

**dm_verity 重点结论：**
- offset 0x10 (`critical_lock_state`) 的处理在两个版本中**完全一致**
- pack 格式都是 `"<IIIIIII"`（7 个 u32）
- unlock 时 `critical_lock_state = 1`，lock 时 = `0`
- SHA256 hash 计算覆盖 28 字节头部
- **dm_verity bug 没有修复**，与 Rust 版当前行为一致

---

### 2.3 `Library/Exploit/kamakiri2.py` [优先级: 低]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| 进度显示 | `print_progress()` | **新 `progress()` 类** | 低 — 仅 UI |
| `patchda1_and_da2()` | 仅 sha1/sha256 | **新增 MD5 (hashmode==0) 支持** | 中 — 需扩展哈希算法 |
| 整体逻辑 | 不变 | 不变 | 无 |
| 版权年份 | 2024 | 2025 | 无 |

---

### 2.4 `Library/mtk_preloader.py` [优先级: 高]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| **`init()` 参数** | `(maxtries=None, display=True)` | `(display=True, directory=None)` — **移除 maxtries** | 中 — 接口变更 |
| **IoT 模式检测** | `if self.config.iot:` 分支 | **hwcode 读取失败时自动检测为 IoT** | **高** — 新设备类型支持 |
| IoT 芯片初始化 | 无 | **~70 行 RTC/GPIO/EMI/PMU 配置** | **高** — 新协议流程 |
| **`send_env_prepare()` 新方法** | 无 | **全新复杂握手协议（0x5A、0x69、0x00）** | **高** — 必须实现 |
| **`dump_internal_flash()` 新方法** | 无 | 分页读取内部闪存 | 中 — 新功能 |
| **`read32_direct()` 新方法** | 无 | 直接读 32 位 | 低 |
| `read()` | 无 direct 参数 | **新增 `direct: bool = False`** | 低 |
| `brom_register_access()` | `mode == 0` | **`mode in [0, 2]`** + 新增 `mode=-1` 参数 | 中 — 模式扩展 |
| `jump_da()` | 无延迟 | **新增 `sleep(0.1)`** | 低 |
| `upload_data()` 块大小 | 固定 64 字节 | **动态 EP_IN.wMaxPacketSize**，hwcode 0x2531 跳过最终空包 | 高 |
| `setreg_disablewatchdogtimer()` | 通用处理 | **新增 IoT 芯片看门狗禁用分支** | 中 |
| `send_auth()` 块大小 | 硬编码 64 | **动态 EP_IN.wMaxPacketSize** | 中 |

---

### 2.5 `Library/DA/mtk_da_handler.py` [优先级: 极高]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| 行数 | ~944 | ~1602 (**+658 行**) | **极高** |
| `configure_da()` | `(da, preloader)` | **`(da)` — 从 mtk.config 获取 preloader** | 中 |
| **eFuse 操作（~400+ 行新增）** | 无 | **efuse_runtime_blow_main_handler、pwrap_read/write、pmic_config_interface 等** | **极高** — 全新硬件协议 |
| **IMEI/NV 处理** | 无 | **imei 读/写、nvitem 加解密、CSSD 处理** | 高 |
| **`zeroization()`** | 无 | 密钥擦除/归零化 | 低 — 安全功能 |
| `da_rf()` | `daconfig.flashtype` | **`daconfig.storage.flashtype`** + UFS LU 大小 | 高 — 存储结构变更 |
| `da_read()` | 无 offset/length | **新增 offset、length、display 参数** | 中 |
| `da_erase()` | 通用擦除 | **新增 boot1/boot2 分区格式化** | 中 |
| `da_peek()` | pagesize 0x200 | **pagesize 0x20000** | 中 |
| memdump 地址 | brom 0x200000 | **brom 0x300000** | 低 |
| 新增命令子项 | 无 | **keyserver、nvitem、patchmodem、imei、rpmb auth** | 高 |
| `handle_da_cmds()` | 基础命令 | **大幅扩展**，支持 IoT、eFuse、IMEI | **极高** |

---

### 2.6 `config/brom_config.py` [优先级: 中]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| 芯片新增 | - | 0x1209(MT6835V/ZA), 0x1236(MT6989W), 0x1375(MT6878), 0x6899(MT6899), 0x1471(MT6993) | 中 — 需更新配置 |
| IoT 芯片 | 无 | **20+ 个 IoT 芯片**（MT2523/2625/5932/7682/7686/6225/6226/6236/6238/6253/6255/6256/625a/6268/6270/6276/6291） | 中 |
| Chipconfig 字段 | 无 iot | **新增 `iot = False`** | 低 |
| eFuse 配置 | 基础 | **hwcode 0x1209 完整 eFuse 映射，internal_fuses/external_fuses 列表** | 中 |
| **MT6771/MT8183 (0x788)** | **不变** | **完全未变** | **无** |

---

### 2.7 `Library/Connection/usblib.py` [优先级: 低]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| USB 调试日志 | `usb_debug_log()` 等 | **全部移除** | 无 — 不需要 |
| `connect()` 设备过滤 | 简单匹配 | **VID 白名单过滤** (0x0E8D/0x1004/0x22d9/0x0FCE) | 低 |
| `set_line_coding()` | 直接 ctrl_transfer | **新增 isFtdi 参数，try-except 容错** | 低 |
| `usbread()` | 无 early break | **maxtimeout==-1 时 early break** | 低 |
| `ctrl_transfer()` | 带日志 | **简化，移除日志** | 无 |

---

### 2.8 `Library/DA/xflash/extension/xflash.py` [优先级: 高]

| 变更项 | v2.0.1 | v2.1.4.1 | Rust 影响 |
|---|---|---|---|
| `patch_da2()` | 基础补丁 | **新增 Techno hash binding、moto_disable_sla** | 中 |
| `custom_rpmb_init()` | 无 rpmbkey | **新增 rpmbkey 参数，hwcode 0x1209/0x1129 mode=1** | 中 |
| `seccfg()` | 不传 custom_sej_hw | **传递给 SecCfgV3/V4** | 中 |
| `generate_keys()` | 基础密钥 | **新增 motokey/HRID MD5/SHA256/rpmbkey6580** | 中 |
| **`custom_sej_hw()` 新方法** | 无 | SEJ 硬件加密（多种模式） | **高** — 需硬件或模拟 |
| **`nvitem()` 新方法** | 无 | NV 项 SW/HW AES 加解密 | 高 |
| **`auth_rpmb()` 新方法** | 无 | RPMB 认证 | 中 |
| **`keyserver()` 新方法** | 无 | TCP 密钥服务器 | 低 — 网络功能 |
| `read_fuses/peek` | 无 registers | **`registers=True` 参数** | 低 |

---

## 3. 优先级汇总

### 高优先级（需要立即同步到 Rust 移植）

| 文件 | 关键变更 | 影响 |
|---|---|---|
| `xflash_lib.py` | `ack()` MT6781 单包协议 | 需按芯片差异化 |
| `xflash_lib.py` | `send_data()` 动态包大小 | 需查询 USB 端点信息 |
| `xflash_lib.py` | `writeflash()` 无条件 512 对齐 | 对齐逻辑需更新 |
| `xflash_lib.py` | `readflash()` 分块读取+超时+异常清理 | 大文件读取可靠性 |
| `xflash_lib.py` | 存储管理重构（storage 对象） | 分区信息获取方式变更 |
| `xflash_lib.py` | SLA 绕过策略增强（厂商签名） | 新设备兼容性 |
| `xflash_lib.py` | Carbonara exploit 集成 | 新漏洞利用路径 |
| `mtk_preloader.py` | IoT 模式自动检测 + 初始化 | 新设备类型 |
| `mtk_preloader.py` | `send_env_prepare()` 握手协议 | 必须实现 |
| `mtk_preloader.py` | 动态 USB 包大小 | 数据传输效率 |
| `extension/xflash.py` | SEJ 硬件加密 | 新密钥类型 |

### 中优先级（后续跟进）

| 文件 | 关键变更 | 影响 |
|---|---|---|
| `seccfg.py` | V3 `verify()` 新方法（0x850 偏移检测） | 容错性提升 |
| `mtk_da_handler.py` | eFuse 烧录操作 | 新功能模块 |
| `mtk_da_handler.py` | IMEI/NV 加解密 | 设备管理功能 |
| `brom_config.py` | 新增 25+ 芯片配置 | 设备兼容性 |
| `extension/xflash.py` | NV 项加解密 | 设备管理 |

### 低优先级（无关紧要）

| 文件 | 关键变更 | 影响 |
|---|---|---|
| `kamakiri2.py` | 进度条 API 变更 | 仅 UI |
| `usblib.py` | 日志移除、设备过滤 | 不影响核心协议 |
| `seccfg.py` | custom_sej_hw 死代码 | 未启用功能 |

---

## 4. 重点关注结论

### 4.1 dm_verity bug 状态
**两个版本的 `critical_lock_state` (offset 0x10) 处理完全一致，没有修复。** pack 格式、值设置（unlock=1, lock=0）、SHA256 hash 计算逻辑都相同。Rust 版当前的行为是正确的（C# 版行为：只修改 lock_state(0x0C)，不动 critical_lock_state(0x10)）。

### 4.2 ACK 两段写逻辑
- v2.0.1：所有芯片统一两段写（12B 头 + 4B 数据）
- v2.1.4.1：MT6781 改为 16B 单包，其他芯片保持两段写
- **Rust 版当前使用两段写，与 v2.0.1 一致，对 MT6768 等芯片没问题**
- 如需支持 MT6781，需要增加单包变体

### 4.3 EMI 发送时序
两个版本基本一致，v2.1.4.1 移除了调试日志，错误提示更明确。**Rust 版当前实现正确。**

### 4.4 Storage 重构
v2.1.4.1 将所有存储管理（EMMC/UFS/NAND/NOR）从 `xflash_lib.py` 内部方法提取到独立的 `storage.py` 模块。这是一个**架构级重构**，Rust 版当前使用内联方式获取分区信息，建议后续考虑类似的抽象层。

---

## 5. 对 Rust 移植的行动建议

### 立即实施
1. `ack()` 增加 MT6781 单包变体（按 dacode 判断）
2. `send_data()` / `upload_data()` 改用动态 `wMaxPacketSize`
3. `write_partition()` 确保无条件 512 字节对齐
4. `readflash_data()` 增加分块读取和超时处理
5. 存储管理考虑抽象为独立模块

### 规划中
6. IoT 模式检测和初始化流程
7. `send_env_prepare()` 握手协议
8. SLA 绕过厂商签名支持
9. V3 seccfg `verify()` 方法
10. Carbonara exploit 集成（如需）

### 暂不实施
11. eFuse 烧录操作（硬件依赖重）
12. IMEI/NV 加解密（功能扩展）
13. SEJ 硬件加密（需要底层支持）
14. GUI 相关（与 CLI 工具无关）
