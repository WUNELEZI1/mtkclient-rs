# Kamakiri2 与 Carbonara 漏洞执行方案对比

> 参考资料：
> - [blog.r0rt1z2.com/posts/dissecting-a-mantis/](https://blog.r0rt1z2.com/posts/dissecting-a-mantis/) — Kamakiri 内部机制剖析
> - [blog.r0rt1z2.com/posts/exploiting-mediatek-datwo/](https://blog.r0rt1z2.com/posts/exploiting-mediatek-datwo/) — Carbonara 后续与 heapb8
> - [itssho.my/blog/article/serving-carbonara](https://itssho.my/blog/article/serving-carbonara) — Carbonara 起源故事
> - `mtkclient-2.0.1/mtkclient/Library/Exploit/kamakiri2.py` — 项目对照 Python 实现
> - `mtkclient-2.1.4.1/mtkclient/Library/Exploit/carbonara.py` — Carbonara Python 实现

## 0. 漏洞分层概念

mtkclient 漏洞利用按执行层级分为：

| 层级 | 触发阶段 | 漏洞类型 | 公开 exploit |
|------|---------|---------|--------------|
| BROM 层 | BootROM USB 栈 | 控制传输、TX buffer spray | Kamakiri / Kamakiri2 / Amonet |
| DA 层 | DA1 内存 | hash 重写、boot_to 重放 | Carbonara |
| DA2 层 | heap 溢出 | USB 下载 handler | heapb8 (2026 新公开) |

本项目 `mtkclient-rs` 现状：**Kamakiri2 已实现**（src/kamakiri2.rs），**Carbonara 未实现**（无对应 Rust 模块）。

---

## 1. 任务 A：Kamakiri2 原理与 Rust 移植对照表

### 1.1 漏洞原理（来自 blog.r0rt1z2）

Kamakiri 利用 BROM 的 class-specific USB 控制传输处理逻辑：

```
BROM 的 USB 栈（自下而上）：
  USB FIFO 硬件 → ACM staging buffer → IO_PutData32_Ex 序列化
                                       ↓
                                  IO_GetData/IO_PutData 函数指针表
                                       ↓
                                  BromCmdLoop（dispatch 命令）
```

**核心漏洞点**：`USB_Endpoint0_Idle` 用 `cmd.wIndex` 直接索引 `if_info[]` 表调用 `if_class_specific_hdlr`，但**没有范围检查**。攻击者可以指定任意 `wIndex` 值，让 BROM 跳转到 `usbacm_tx_buf` 中的某个字节，把它当作函数指针执行。

**攻击步骤**（来自 PoC）：
1. **handshake**：`A0 0A 50 05` → BROM 返回按位取反
2. **TX buffer spray**：向 `WDT_BASE+0x50 = 0x10007050` 写入 `0x000A1000`（payload 地址），然后用递增的 `read32` 把这个值"挤"到 `usbacm_tx_buf` 中 `if_info[wIndex].if_class_specific_hdlr` 偏移处
3. **payload 上传**：用 `BromCmd_SendCert(0xE0)` 把任意代码加载到 `0x100A00`
4. **触发**：发控制传输 `(0xA1/0x21, 0, wIndex=overshoot)` → BROM 把 `0x000A1000` 当函数指针调用

### 1.2 Kamakiri2 与 Kamakiri 区别

| 项 | Kamakiri（原始） | Kamakiri2 |
|----|-----------------|-----------|
| 输入接口 | USB control transfer (类请求) | USB control transfer (类请求) |
| 关键控制传输 | `0xA1, 0x25, 0, 0` GET_LINE_CODING | `0xA1, 0x21, 0, 0, 7` GET_LINE_CODING |
| Payload 触发 | TX buffer spray + if_info 越界 | 直接 `0x21, 0x20, 0, 0` SET_LINE_CODING 修改 linecode 缓冲 |
| 易用性 | 较复杂（需要精确 offset） | 简单（linecode buffer 修改） |
| 项目状态 | mtclient 有 | 项目已实现 |

**Kamakiri2 关键洞察**（来自代码）：它不需要 TX buffer spray，而是利用 BROM 的 **linecode staging buffer** 溢出 —— 通过 `SET_LINE_CODING (0x21, 0x20)` 写入超过 7 字节的数据，超出部分会进入相邻的 BROM 内存（stack/heap 区域），然后被 `brom_register_access` 当作地址使用。

### 1.3 Python 实现函数清单（`kamakiri2.py`）

| Python 函数 | 行号 | 职责 |
|------------|------|------|
| `kamakiri2(addr)` | 23-40 | 单步：linecode + 4 字节地址 → ctrl_transfer(0x21,0x20) |
| `da_read_write(addr, len, data, check)` | 42-117 | 核心读写：da_setup + 3/4 步 setup + brom_register_access |
| `exploit(payload, payloadaddr)` | 119-133 | payload 注入：da_write(payload → addr) + da_write(addr → ptr_send) |
| `bruteforce(args, startaddr)` | 135-159 | BROM 起始地址爆破 |
| `newbrute(dump_ptr, dump)` | 161-209 | 单步试探 / dump 模式 |
| `dump_brom(filename, dump_ptr, length)` | 211-236 | 流式 BROM dump |
| `dump_preloader(filename)` | 238-253 | preloader dump 入口 |
| `payload(payload, daaddr)` | 255-276 | payload 包装（4 字节对齐 + hwcrypto disable） |
| `runpayload(payload, ack, addr, dontack)` | 278-290 | 注入 + 等待 ack |
| `patchda1_and_da2()` | 292-321 | patch DA1+DA2（计算 hash 并回填） |
| `da_read(addr, len)` | 62 | 包装只读 |
| `da_write(addr, len, data)` | 65 | 包装写入 |

### 1.4 Rust 现状（`src/kamakiri2.rs`）

| Rust 函数 | 行号 | 对应 Python | 完成度 |
|----------|------|------------|--------|
| `kamakiri2_step(lc, ptr_da_bra, addr)` | 42-57 | `kamakiri2()` | ✅ 已实现（含 libusb/serial 分支） |
| `da_setup(lc, ptr_da_bra, watchdog)` | 59-69 | `da_read_write` 头部 | ✅ |
| `da_read(lc, ptr_da_bra, ptr_da, watchdog, addr, len)` | 71-119 | `da_read_write` | ✅ |
| `da_write(lc, ptr_da_bra, ptr_da, watchdog, addr, data, check)` | 121-190 | `da_read_write` | ✅ |
| `read_payload_address(...)` | 192-202 | exploit 中 ptr_send 计算 | ✅ |
| `inject_payload(payload, expected_ack)` | 204-278 | `exploit()` | ✅ |
| `run_kamakiri2()` | 282-327 | `runpayload()` | ✅ |
| `run_payload(filename, ack)` | 331-339 | `payload()` 文件入口 | ✅ |
| `run_payload_from_data(payload, ack)` | 341-347 | `payload()` 内存入口 | ✅ |
| `dump_preloader_from_ram(debug)` | 350-449 | `dump_preloader()` 简化版 | ✅（路径不同） |
| `bypass_security(context)` | 451-482 | run_handshake + payload | ✅ |
| `run_dump_brom_payload(filename, debug)` | 485-497 | `dump_brom` payload 路径 | ✅ |
| `dump_preloader_payload(debug, quiet, context)` | 502-638 | `dump_preloader()` 完整版 | ✅ |
| `dump_brom(debug)` | 640-669 | `dump_brom` 内层 | ✅ |

### 1.5 Rust 缺失项

| 缺失项 | Python 位置 | 影响 |
|--------|------------|------|
| `bruteforce()` / `newbrute()` | kamakiri2.py:135-209 | 不支持 BROM 地址爆破（高级功能） |
| `patchda1_and_da2()` hash 回填 | kamakiri2.py:292-321 | DA1/DA2 patch 不计算 hash |

### 1.6 关键差异点（Rust 增强项）

1. **linecode 来源**：
   - Python：`ctrl_transfer(0xA1, 0x21, 0, 0, 7)` + push 0 → 8 字节
   - Rust `inject_payload`：相同操作（kamakiri2.rs:218-220）
   - **已对齐**

2. **da_setup 步骤数**：3 步 `ptr_da_bra+5/+6/+7`，Rust 与 Python 一致

3. **addr < 0x40 vs >= 0x40 分支**：
   - addr<0x40 → 4 步 (-6, -5, -4, -3)
   - addr>=0x40 → 3 步 (-5, -4, -3) + bra_addr = addr - 0x40
   - **已对齐**

4. **payload 注入顺序**：da_write(payload → addr) → da_write(addr → ptr_send, check=False)，**已对齐**

5. **wait ack 逻辑**：5 秒超时 + read_exact，**已对齐**

6. **串口/USB 双路径**：Rust 增加了 serial mode 跳过 kamakiri2 steps 的分支，**Rust 比 Python 更强**

### 1.7 任务 A 小结

✅ **Kamakiri2 核心功能 100% 已移植**（含 dump preloader、dump brom、bypass_security、payload 注入）。
⚠️ 仅缺 `bruteforce` 和 `patchda1_and_da2` 两个离线/高级功能。
✅ 串口/USB 双路径支持是 Rust 版的增强项。

---

## 2. 任务 B：Carbonara 原理与 Rust 移植对照表

### 2.1 漏洞原理（来自 itssho.my 博客）

Carbonara 利用 **DA2 hash 校验可重写** 的特性：

```
DA1 加载阶段（被 DAA 强制 hash 校验保护）：
  host → boot_to(da2_addr, da2_size, da2_hash) → DA1 校验 → 跳到 DA2

Carbonara 攻击：
  1. host 第一次发 boot_to(empty payload)        // 0xA4DE22... 的位置指向 hash 槽
  2. host 第二次发 boot_to(patched_da2, size)    // DA1 此时把 patched_da2 写入
  3. host 第三次发 da2_patched_hash              // 这个 hash 实际写到第 1 步定位的地址
  4. host 第四次发 boot_to(da2_addr, patched_da2_size, new_hash)
       // DA1 重新计算 patched_da2 hash，发现 = new_hash，校验通过！
```

**为什么能工作**：
- BROM 不检查 `DA2 load address` 是否合理（`0x20000000` 任意）
- DA1 的 `boot_to` handler 把 `da2_hash` 字段**先按 `da2_addr + da2_size` 写回 DRam**，再校验
- 第 1 步用空 payload 试探出 DA1 写入 hash 的实际地址
- 第 3 步把"正确 hash"覆盖到那个位置
- 第 4 步 DA1 重新算 hash，与覆盖值匹配

**安全研究员 (shomy) 12 行 PoC**（xflash_lib.py line 1226）：

```python
if self.xsend(self.Cmd.BOOT_TO):
    payload = bytes.fromhex('a4de2200000000002000000000000000')  # 试探写入
    if self.xsend(payload):
        if self.status() == 0:
            import hashlib
            da_hash = hashlib.sha256(self.daconfig.da2).digest()
            if self.xsend(da_hash):     # 覆盖 hash 槽
                self.status()
                self.info("All good!")
```

### 2.2 Python 实现函数清单（`mtkclient-2.1.4.1/carbonara.py`）

| Python 函数 | 行号 | 职责 |
|------------|------|------|
| `check_for_carbonara_patched(data)` | 14-35 | 通过 5 个特征字节序列检测 V5/V6 设备是否已修补 Carbonara |
| `patchda1_and_upload_da2()` | 37-109 | 完整 patch 流程：da1 → check patched → stock da2 → boot_to(hash) + boot_to(da2) |

### 2.3 Carbonara 关键点

**触发条件**：
- 设备 SLA 校验通过
- DA1 region 中没有以下任一特征字节：
  - V6 patch1: `\x01\x01\x54\xE3\x01\x14\xA0\xE3`
  - V6 patch2: `\x08\x00\xa8\x52\xff\x02\x08\xeb`
  - V6 patch3: `\x01\x01\x50\xE3\x01\x14\xA0\xE3`
  - V6 patch4: 字符串 "2nd DA address is invalid"
  - V5 patch: `\x06\x9B\x4F\xF0\x80\x40\x02\xA9`

**核心机制**：
- **V6**：用 `daloader.boot_to(addr, hash)` + `daloader.boot_to(da2_addr, da2_patched)` 写 hash + 加载 DA2
- **V5**：用 SHA1/SHA256/MD5 计算 da2patched 前 hashlen 字节的 hash，patch 到 da1 的 hashaddr 位置

### 2.4 Rust 现状

| Rust 位置 | 内容 | 缺失 |
|----------|------|------|
| `src/da_xflash.rs::upload_da2` | 读 da1 region、patch da2、boot_to | ❌ **没有** Carbonara patch 检测 |
| `src/da_xflash.rs::patch_da1` | 简单的 boot patch | ❌ 没有 hash 回填 |
| 项目内 `carbonara.rs` | **不存在** | ❌ **整模块缺失** |
| 项目内 `exploit_handler.rs` 状态机 | 不存在 | ❌ 漏洞选择逻辑散落在 da_xflash.rs |

### 2.5 Carbonara Rust 移植步骤

1. **新增 `src/carbonara.rs`**：
   ```rust
   pub fn check_for_carbonara_patched(da1: &[u8]) -> bool
   pub fn patchda1_and_upload_da2(&mut self, da1: &[u8], da2: &[u8]) -> Result<()>
   ```

2. **修改 `src/da_xflash.rs::upload_da2`**：
   - 在 patch da1 后调用 `check_for_carbonara_patched(da1_patched)`
   - 如果 patched → 跳过 Carbonara，走原路径
   - 如果未 patched → 调用 `patchda1_and_upload_da2()` 走 Carbonara 路径

3. **新增 `src/exploit_handler.rs`**（状态机）：
   - 抽象 `Exploitation` trait
   - `Kamakiri2`、`Carbonara`、`Insecure` 三个实现
   - 决策表：`hassecurity` ? Carbonara : Insecure；`v6` ? Carbonara : Kamakiri2

4. **修改 `Cargo.toml` + `main.rs`**：
   - 引入 `sha2`、`sha1`、`md5` 依赖

### 2.6 Carbonara 集成工作量评估

| 步骤 | 工作量 | 风险 |
|------|--------|------|
| 新建 carbonara.rs 基础结构 | 0.5 天 | 低（5 个特征字节 + 1 个主流程） |
| 接入 upload_da2 | 0.5 天 | 中（需要保留 fallback） |
| hash 回填（v5/v6 分支） | 1 天 | 中（需对齐 Python 的 compute_hash_pos） |
| exploit_handler 状态机 | 1-2 天 | 高（影响所有漏洞选择路径） |
| 实机测试 | 1 天 | 取决于设备 |

**总工作量**：3-5 天

### 2.7 与项目契合度

- ✅ **不需要新 USB 通信逻辑**：Carbonara 在 DA 模式下用 boot_to
- ✅ **不需要新 payload 文件**：与 Kamakiri2 完全不同（patch DA1 字节码）
- ✅ **可作 Kamakiri2 失败时的 fallback**：patched 设备的 SLA bypass
- ✅ **与 da_extension 流程正交**：Carbonara 在 DA1 加载时介入，extensions 在 DA2 后介入

### 2.8 任务 B 小结

❌ **Carbonara 完全未实现**，需新建模块 + 改 da_xflash.rs + 可能引入 exploit_handler 抽象。
✅ 移植可行性高（不涉及 USB 通信），需新增 hash 依赖。
✅ 优先级：中等（Kamakiri2 已覆盖大部分场景，Carbonara 主要解决 V5/V6 patched 设备的 SLA bypass）。

---

## 3. 整体结论

| 维度 | Kamakiri2 | Carbonara |
|------|-----------|-----------|
| Rust 移植完成度 | ~95% | 0% |
| 主要缺失 | bruteforce、patchda1_and_da2 | 整个模块 |
| 集成难度 | 低（仅函数补全） | 中（需新模块+主流程集成） |
| 优先级 | 低（功能已完整） | 中（扩展 patched 设备支持） |
| 引入新依赖 | 无 | sha2/sha1/md5 |

**建议下一步**：先做 Carbonara 任务 B 的步骤 1（新建 carbonara.rs 基础结构），低风险、易落地。