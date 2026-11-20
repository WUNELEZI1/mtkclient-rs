# 架构落地状态（对应「Agent 执行版」规划）

## 一、本轮实际落地范围

规划要求的目标目录是 `core / transport / da / exploit / ops / fs / slot / logging / confirm` 的分层结构。
当前仓库已有 35k 行、219 个测试，**一次性整体搬迁会同时破坏现有实现与测试基线**，因此本轮采用
「**新增单向依赖层 + 渐进迁移**」：

| 规划层 | 当前状态 | 位置 |
|---|---|---|
| `core/`（能力格 / 安全位 / 设备状态 / 规划模型） | ✅ 新建 | `src/core/{capability,safety,device,plan}.rs` |
| `transport/`（StreamPort / BromPort / mock） | ✅ trait + mock 新建；具体后端仍用现有 `usb` / `preloader::transport` | `src/transport/{mod,mock}.rs` |
| `exploit/registry` | ✅ 新建（单点注册表） | `src/exploit/registry.rs` |
| `exploit/planner` | ✅ 新建（BFS + mode 约束 + V6 排除 + 条件 probe + Cost） | `src/exploit/planner.rs` |
| `exploit/orchestrator` | ✅ 新建（决策 + JSON 行日志） | `src/exploit/orchestrator.rs` |
| `da/fixture` | ✅ 新建（六段粒度 + JSONL + mock 回放） | `src/da/fixture.rs` |
| `confirm/` | ✅ 新建（两种模式：`--yes` 直接放行 / 不加则风险提示 + 交互式 y/N，无 token） | `src/confirm/mod.rs` |
| `ops/` | ✅ **全部迁完**：`printgpt` / `read` / `write` / `erase` / `seccfg` / `vbmeta` / `unlock` / `fs_shell`，每个只声明能力 + 实现 `run` | `src/ops/*.rs`（契约见 [`ops.md`](ops.md)） |
| `exec/`（唯一执行入口） | ✅ 新建：决策 → 逐步执行 → **每步校验能力真实增长** | `src/exec/mod.rs` |
| `session/`（执行后端抽象） | ✅ 新建：`RealSession`（DAXFlash）/ `SimSession`（虚拟设备） | `src/session/{mod,real}.rs` |
| `da/engine`（StreamPort 协议引擎） | ✅ 新建：握手 → DA 上传 → 开存储 → 读 flash，全走 `&mut dyn StreamPort` | `src/da/engine.rs` |
| `transport/vdev`（虚拟设备） | ✅ 新建：BROM + XFlash 设备侧状态机，画像驱动，含故障注入 | `src/transport/{vdev,wire}.rs` |
| `fs/` | ✅ 已从 `system/fsimg/` 迁出并拆为 `fs/f2fs/{mod,layout}.rs`、`fs/ext4/{mod,tests}.rs`，全部 ≤500 行（2026-11-20） | `src/fs/` |
| `slot/` | ✅ 2026-11-20 从 `partition/slot.rs` 提升为顶层 `slot/mod.rs`；命令层 `cmd/io/slot.rs` 保留 | `src/slot/mod.rs` |
| `logging/` `cli/` | ⏳ 复用现有 `progress`+`i18n` / `cmd::cli` | — |

依赖方向：`core ← transport ← da ← exploit ← cmd`，`core` 不引用任何内部模块（`Stage` 定义在 core，避免反向依赖）。

## 二、第一批之后的进展：能力驱动闭环已可运行

```
mtkclient-rs --sim v5-clean printgpt
```

`printgpt` 不再有硬编码流程，而是：

1. `PrintGptOp::required_caps() = StorageRead | GptParsed`；
2. `Executor` 把它交给 `Orchestrator.decide()`，得到 `Plan` 与决策日志；
3. 按 `PlanStep` 调用 `Session`（真机 = `DAXFlash`，仿真 = `DaEngine + VirtualDevice`）；
4. **每一步都校验能力确实增加**（`ExecError::NoProgress`），收尾核对 `required ⊆ acquired`
   （`ExecError::Incomplete`）。

因此"日志里写的路径"和"实际跑的路径"是同一份 `Plan`，不会再漂移。

真机路径同样接到 Executor：`dispatch_cmd("printgpt")` → `RealSession`（DA 已被 Phase1 上传，
故声明 `DaLoaded`，规划只剩"开存储 → 解析 GPT"）。详细说明见 [`simulation.md`](simulation.md)。

## 三、本轮：读/写/擦/高危操作全部收口到 Executor

前面几轮只把 `printgpt` 接进了能力闭环；这一轮把**真正会动 flash 的操作**也接了进来，
并且全部在仿真下跑通（无真机）。新增/改造：

| 能力 | 落点 | 关键实现 |
|---|---|---|
| 读 | `ops/read.rs` | 1 MiB 分块；先写 `<out>.part` 再 rename，中途断不产生半个"成功"文件 |
| 写 | `ops/write.rs` | 大小校验**不截断** / 重点分区确认 / checkpoint / 回读校验 |
| 断点续传 | `ops/checkpoint.rs` | 8 MiB 周期落盘、四项指纹、原子 rename、成功后删除 |
| 擦 | `ops/erase.rs` | `FORMAT` 三段 + 擦后抽样断言 0xFF/0x00；幂等 |
| seccfg | `ops/seccfg.rs` | V4 头构造 + 回读断言 `0x0C=3` / `0x10=0` |
| vbmeta | `ops/vbmeta.rs` | 先定槽位（bootctrl → GPT → 报错），只改活动槽，`[SKIP]` 记原因 |
| 一键解锁 | `ops/unlock.rs` | seccfg unlock → vbmeta(flags=3)；vbmeta 失败降级为警告，不掩盖解锁成功 |
| fs_shell | `ops/fsshell.rs` | 只读 `StorageRead`；作为"新增 op 不改 Executor"的扩展性验证 |

支撑改动：

- `da/engine.rs` 补 `begin_write / write_chunk / end_write / format`（写路径也走 `StreamPort`，
  真机与仿真共用同一份协议代码）；
- `session::{Session, SimSession, RealSession}` 新增 `find_partition / read_flash / write_flash /
  write_stream / erase_partition / active_slot / seccfg_sign / serial`；
- `transport/vdev.rs` 增加设备侧写/擦状态机、稀疏页存储、故障注入（`WriteDead`/`WriteSilent`）、
  `save_state/load_state`（跨进程"重连"）；
- `confirm` 改为两种模式：不加 `--yes` 时打印风险提示 + 交互式 y/N；加 `--yes` 直接放行（静默），供脚本/程序调用。已删除 token 机制。

### 3.1 跨进程断点续传（仿真的"拔插重连"）

```
第一跑  --sim flaky-write --sim-state S w boot_b img   → 10 MiB 处断开，.resume 落盘 0x800000
第二跑  --sim v5-clean    --sim-state S w boot_b img   → 从 0x800000 续写 → 校验通过 → 删 .resume
```

`--sim-state` 把虚拟设备的稀疏页 + 擦除区间落盘，第二跑载入后**存储内容还在**，
这正是真机"线上掉线 → 重新插上"的语义等价物。

## 四、这轮新增的对外能力

```bash
# 离线查看规划，不需要设备
mtkclient plan printgpt --da DA.bin
mtkclient plan printgpt --da DA.bin --safety 0x2     # 模拟 SLA
mtkclient plan printgpt --da DA_V6.bin               # V6：不规划 BROM
mtkclient plan seccfg --da DA.bin --chip 0x6768
```

输出 = 人类可读规划 + 一行结构化 JSON（`{"event":"decision",...}`），
`--log` 级别不影响该 JSON。

## 五、验收元标准对照

| # | 项 | 状态 | 说明 |
|---|---|---|---|
| 1 | printgpt 至少两条路 + 理由 | ✅ | `plan printgpt` 实测 3 步直连 vs 4 步 kamakiri2 |
| 2 | 加 exploit 只改注册表 | ✅ | `registry::BUILTIN` 单点 |
| 3 | V6 不规划 BROM | ✅ | `DeviceState::brom_reachable()` + planner 测试 |
| 4 | Carbonara pattern 命中不规划 | ✅ | 5 条 pattern 单测 + 规划无解日志 |
| 5 | Heapbait SLA 门控 probe → NotVulnerable | ⚠️ 部分 | 已实现 DA2 修补标记检查；`requires_sla` 无法静态验证，改为运行期记录（见 docs/heapbait_sla_gating.md） |
| 6 | seccfg unlock 后偏移 16 == 0 | ✅ | `ops::seccfg` 写后回读断言；`seccfg_unlock_writes_dm_verity_zero` |
| 7 | `--yes` 直接放行（静默）/ 不加则风险提示 + y/N | ✅ | `DangerConfirm` 两种模式；`--yes` 不打印警告，专为脚本/程序调用；非交互无 `--yes` 直接报错提示加 `--yes` |
| 8 | mock 下 DA 协议脱机跑 | ✅ | `DaEngine` + `VirtualDevice` 跑完整 BROM→DA→读写擦状态机（读/写/擦/FORMAT/CC_OPTIONAL_DOWNLOAD_ACT 全覆盖） |
| 9 | A/B 缓存 set_active 后读 boot 新槽 | ✅ | `slot a\|b` 写 misc 后同步 `da.active_slot` |
| 10 | 新增日志走 LogKey | ✅ | help 六语言补齐 `unlock` / `fs_shell`；日志走统一 logger |
| 11-15 | vbmeta 槽位/回退/混合布局 | ✅ | `decide_action` 纯函数 + 三级降级；混合布局与 bootctrl 失败均有测试（`ab-unknown-slot` 画像） |
| 16 | dumpbrom header 自描述 | ⚠️ | 保持"不解析头部长度"（无可靠公开出处）；改为可指定长度 + 2 MiB 默认 + 尾部 0xFF 裁剪告警 |
| 17 | `--mode` 约束 Planner | ✅ | `Mode::allows` + 单测 |
| 18 | `--log 1/2/3` 不影响决策日志 | ✅ 设计 | JSON 独立于 logger |
| 19 | `--log 3` hex dump + 搜索树 | ✅ | USB / 串口 / DA 引擎全带时间戳 hex dump；Planner 输出 BFS 展开节点 + 每条排除分支原因 |
| 20 | 每个 CVE 有结论 | ✅ | `docs/preloader_cve_status.md` |
| 21 | Preloader 只搜 Adapted | ✅ | 注册表无 Adapted → 规划报“无可用 Preloader exploit” |
| 22 | 主/兜底路径文档分开 | ⚠️ | preloader / BROM 的主路径与兜底已在 `docs/preloader_cve_status.md` 分开；`dumpbrom` 兜底路径仍待补文档 |
| 23 | adb-rs 回归 | — | 不在本仓库 |
| 24 | f2fs 生成器在 fs/ | ✅ | 2026-11-20 已迁至 `src/fs/`，`format_image_for` 引用同步更新 |
| 25 | RPMB HACC/SSR 缺口明确 | ✅ | 实测为 SEJ，见 `docs/rpmb_sectors.md` |
| 26 | 芯片表含 V6 / Heapbait 缺口 | ✅ 部分 | `KNOWN_V6_DA_CODES` + docs |
| 27 | Cost 定义写进注释 | ✅ | `core/plan.rs` 模块注释 |

## 六、下一步（按依赖顺序）

1. ~~ops 迁移~~ ✅ `printgpt` / `r` / `w` / `e` / `seccfg` / `vbmeta` / `unlock` / `fs_shell`
   全部走 `Executor`，每个只声明 `required_caps` + 实现 `run`。
2. ~~seccfg 偏移 16~~ ✅ unlock/lock 均写 0 并有回读断言。
3. ~~vbmeta 槽位化~~ ✅ 只改活动槽，`[SKIP]` 记原因，槽位不可判定直接报错。
4. ~~A/B 缓存失效~~ ✅ 写 misc 后同步 `da.active_slot`。
5. ~~写入路径收口 StreamPort~~ ✅ `DaEngine::begin_write/write_chunk/end_write/format`
   让真机与仿真共用一份写/擦协议代码；`RealSession` 仍是 `DAXFlash` 的薄封装。
6. ~~断点续传~~ ✅ checkpoint + `--sim-state` 跨进程续写，均有测试。
7. **dumpbrom header 自描述**：⏸ 仍不做。MTK BROM 头部长度字段无可靠公开出处，
   在无真机/固件对拍的情况下写死偏移属于"猜协议"；保持"以 DA 自报/显式长度为准"。
8. **目录收口**：`system/fsimg → fs/` ✅、`slot/` 独立 ✅（均 2026-11-20）；`cmd/ops` 进一步拆分待办。
9. **RPMB 与 V6(XML)**：仿真侧未覆盖（RPMB 实测为 SEJ，见 `docs/rpmb_sectors.md`）。
10. **真机回归**：以上全部为离线仿真验证，真实设备上仍需按 `docs/simulation.md`
    的同一组命令跑一遍对拍（尤其是写包长与 FORMAT 三段握手）。

## 七、禁区遵守情况

- 未新增 exploit 解决“链少”；`heapbait`/`kamakiripl` 保持 `InProgress`，不进自动规划。
- `chain` 保留为调试视图，未作为必需命令。
- `printgpt` 未硬编码“先加载 DA”，V6 由 Planner 排除。
- `--yes` 是静默放行（专为程序调用），不加 `--yes` 才会提示风险并交互式问 y/N。
- 未以 bkerler 原版为正确性标准；Carbonara 5 pattern、RPMB 用 SEJ 等结论均来自源码实测，并在 docs 记录差异。
- 未引入 clap 之外的重依赖；JSON 手写、bitset 自研。
