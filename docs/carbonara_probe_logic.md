# Carbonara probe 逻辑（实测源码核对）

## 结论

1. **判定方式**：对 **DA1 二进制**做字节 pattern 搜索，**任一命中即判定“已修补”**，返回不可用。
2. **pattern 数量不是 4 个**：
   - penumbra `core/src/exploit/carbonara.rs` 用 **4 个**（3 个 V6 + 1 个 V5）；
   - mtkclient `Library/Exploit/carbonara.py` 用 **5 个**（4 个 V6 + 1 个 V5），多一条 `01 01 50 E3 01 14 A0 E3`；
   - 两边的 `2nd DA address is invalid` 字符串长度不同：penumbra 带句点（末尾 `.`），mtkclient 不带。**移植时以 mtkclient 的 5 条为准，其余 4 条是它的子集**。
3. **Two BOOT_TO 存在，但 DA2 地址不是硬编码 0x40000000**：`da2address` 取自 loader `region[2].m_start_addr`；patched DA2 失败会**回退 stock DA2** 再试一次，并置 `patch = False`。
4. **patch 失败要报“DA 版本不匹配”**：mtkclient 在找不到 `MTK_DA_V5.bin` / `MTK_DA_V6.bin` 或对应 dacode/版本条目时直接 `return False`，上层只看到“失败”，不区分原因。我们的 probe 应把“pattern 命中”和“找不到匹配 stock DA”分开报错。

## 证据

mtkclient `mtkclient/Library/Exploit/carbonara.py`：

```python
def check_for_carbonara_patched(self, data):
    test = data.find(b"\x01\x01\x54\xE3\x01\x14\xA0\xE3")      # V6
    test = data.find(b"\x08\x00\xa8\x52\xff\x02\x08\xeb")      # V6
    test = data.find(b"\x01\x01\x50\xE3\x01\x14\xA0\xE3")      # V6（penumbra 缺失）
    test = data.find(b"2nd DA address is invalid")             # V6，无句点
    test2 = data.find(b"\x06\x9B\x4F\xF0\x80\x40\x02\xA9")     # V5
```

同文件 `patchda1_and_upload_da2()`：

```python
hashaddr, hashmode, hashlen = compute_hash_pos(da1, da2, da1sig_len, da2sig_len, v6)
self.mtk.daloader.boot_to(da1address + hashaddr, dahash)   # 第 1 次 BOOT_TO：改写 hash
if self.mtk.daloader.boot_to(da2address, da2patched):      # 第 2 次 BOOT_TO：送 patched DA2
    ...
else:
    return self.mtk.daloader.boot_to(da2address, da2)      # 回退 stock DA2
```

penumbra `core/src/exploit/carbonara.rs`：

```rust
fn is_vulnerable(&self, da1: &[u8]) -> bool {
    let tests: [&[u8]; 4] = [ ... ];   // 注意：4 条
    for pattern in tests.iter() {
        if memmem::find(da1, pattern).is_some() { return false; }
    }
    true
}
```

## 对 mtkclient-rs 的落地要求

- `probe(state, Some(da_binary))`：
  - `da_binary` 为空 → `Confidence::Unknown`（不能默认可用）；
  - 5 条 pattern 任一命中 → `NotVulnerable`；
  - 未命中 → `High`（DA1 未修补）；
  - 是否真正可 patch 还需 `hash_offset` 可定位；定位不到 → `NotVulnerable` 并给出 `reason`。
- `--exploit carbonara` 强制指定时，即使 `NotVulnerable` 也只打印原因并中止，不静默回退。
- 不使用 `0x40000000` 作为 DA2 地址常量。

## 未覆盖

- “目标 DA1 是否接受 host 提供的 DA2 load address”：源码显示地址来自 loader 自身解析，未发现硬编码；不同 DA 包是否一致仍需真机验证。此项保留为待验证，不写死结论。

## 实现现状（Phase 1 专项：carbonara-hardened）

### 1. 两处 pattern 漂移已消除

`da/loader/header.rs::detect_carbonara_patch` 此前只有 3 条，漏了
`2nd DA address is invalid` 与 V6 penumbra 变体；`exploit/registry.rs` 有完整 5 条。
现在 header 直接引用 `CARBONARA_PATTERNS`（单一事实来源），并配套
`CARBONARA_PATTERN_LABELS` 给出可读标签。

### 2. 新增"强制 DA2 加载地址"启发式（只降级为 Low）

> **与最初假设的差异**：有说法称加固 DA1 会把 DA2 加载地址硬编码为 `0x40000000`。
> 但本文档前半部分的实测核对结论是 **`da2address` 取自 loader `region[2].m_start_addr`，
> 并非硬编码 0x40000000**，patched DA2 失败还会回退 stock DA2。
>
> 因此本仓库**不把 `0x40000000` 当作"已修补"的证据**，而是当作"可疑加固"的启发式：
> 命中后 probe 返回 `Confidence::Low`（不进自动规划，但允许 `--exploit carbonara` 显式尝试）；
> 只有 5 条编译特征命中才返回 `NotVulnerable`。

检测两种形态（`da1_forces_da2_addr`）：

| 形态 | 字节 | 说明 |
|---|---|---|
| 常量池（小端） | `00 00 00 40` | ldr 常量 |
| 常量池（大端） | `40 00 00 00` | 少见，但可能 |
| ARM `MOVT rX, #0x4000` | `00 00 40 E3` | 立即数高位装载 |

误报代价可控：`Low` 仍可由用户显式指定 exploit，不会把可用设备卡死；
漏报代价是"以为可用、实际 patch 失败"——由 carbonara 执行前的自检再兜一层。

### 3. 仿真画像 `carbonara-hardened`

```text
mtkclient-rs --sim carbonara-hardened printgpt
```

- 设备：SLA 打开（强制走 bypass 路径），DA1 标记为已加固；
- `synthetic_da(..., hardened=true)` 会写入 V5 补丁特征 + `0x40000000` 常量；
- 预期：`selected = exploit(kamakiri2) -> da_load -> open_storage -> parse_gpt`，
  **任何路径都不出现 carbonara**（有测试 `carbonara_hardened_plan_never_selects_carbonara` 钉死）。
