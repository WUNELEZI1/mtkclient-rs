# Heapbait 可用性与“SLA 门控”（实测源码核对）

## 结论

1. **mtkclient 的判定条件不是 `requires_sla` 标志，而是 DA2 里的错误字符串**：

   ```python
   def is_vulnerable(self) -> bool:
       da2 = self.mtk.daloader.daconfig.da2
       if b"Dest XML file name buffer overflow" in da2:
           self.info("Device is patched against HeapBait")
           return False
       return True
   ```

   即：DA2 包含该字符串 = 厂商已修补 = 不可用。

2. **SLA 不是“门控到不可达”，而是运行时可被内存 patch 关掉**：

   ```python
   sla_off = da2.find(b"DA.SLA\0ENABLED")
   if sla_off != -1:
       self.patch_mem(sla_off, b"DA.SLA\0DISABLE")
   ```

   随后把 `hash_bind`、`vfy_pol`、`dl_pol`、`get_hw_sbc` 四个函数改成 `force_return`。
   所以补丁链是：`EXP-PATCH-MEM` 可用 → 改 SLA 字符串 → 改 4 个校验函数 → 再加载 DA2。

3. **Issue #96 的 `requires_sla` 说法未能在 mtkclient 源码中直接验证**。源码里没有 `requires_sla` 字段，也没有按芯片号拒绝 Heapbait 的分支。可能的真实含义是“XML 层 `SECURITY-SET-ALLINONE-SIGNATURE` 要求 SLA 时，`EXP-PATCH-MEM` 命令本身不可达”，这属于**运行时行为**，不是静态表。

## 对 mtkclient-rs 的落地要求

- `probe(state, Some(da2))` 必须**先查 DA2 字节**，规则：
  - 含 `Dest XML file name buffer overflow` → `NotVulnerable`（reason: patched）；
  - 不含但缺 `DA.SLA\0ENABLED` / 4 个 patch 定位点 → `Unknown`（reason: pattern 定位失败）；
  - 全部定位成功 → `High`。
- **不能**写成“V6 通用路径”。V6 设备的 DA2 若 `requires_sla` 生效，`EXP-PATCH-MEM` 会返回错误码；运行期返回错误时 Orchestrator 必须记录 `heapbait skipped (reason: EXP-PATCH-MEM rejected)` 并回退到“仅 DA 加载 + 只读能力”，不假装成功。
- 注册表 `status = InProgress`：控制序列可运行，但成功率无数据支撑，默认不进自动 Planner；仅 `--exploit heapbait` 显式触发。

## 证据

- `mtkclient/Library/Exploit/heapbait.py`：`is_vulnerable()`（约 549-560 行）、`patch_da()`（约 617-640 行）、`cmd_exp_patch_mem()`（约 571-590 行）。
- penumbra `core/src/exploit/heapbait.rs`（347 行）实现同类流程，命名 HeapB8。
