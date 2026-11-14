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
| `confirm/` | ✅ 新建（token / 会话级 / `--yes` 仍警告） | `src/confirm/mod.rs` |
| `ops/` | ⏳ 仍在 `cmd/`（未搬迁）；`plan` 命令作为入口之一 | `src/cmd/` |
| `fs/` | ⏳ 仍在 `system/fsimg/`（f2fs 生成器尚未迁到 `fs/`，验收项 24 未完成） | `src/system/fsimg/` |
| `slot/` `logging/` `cli/` | ⏳ 复用现有 `partition` / `progress`+`i18n` / `cmd::cli` | — |

依赖方向：`core ← transport ← da ← exploit ← cmd`，`core` 不引用任何内部模块（`Stage` 定义在 core，避免反向依赖）。

## 二、这轮新增的对外能力

```bash
# 离线查看规划，不需要设备
mtkclient plan printgpt --da DA.bin
mtkclient plan printgpt --da DA.bin --safety 0x2     # 模拟 SLA
mtkclient plan printgpt --da DA_V6.bin               # V6：不规划 BROM
mtkclient plan seccfg --da DA.bin --chip 0x6768
```

输出 = 人类可读规划 + 一行结构化 JSON（`{"event":"decision",...}`），
`--log` 级别不影响该 JSON。

## 三、验收元标准对照

| # | 项 | 状态 | 说明 |
|---|---|---|---|
| 1 | printgpt 至少两条路 + 理由 | ✅ | `plan printgpt` 实测 3 步直连 vs 4 步 kamakiri2 |
| 2 | 加 exploit 只改注册表 | ✅ | `registry::BUILTIN` 单点 |
| 3 | V6 不规划 BROM | ✅ | `DeviceState::brom_reachable()` + planner 测试 |
| 4 | Carbonara pattern 命中不规划 | ✅ | 5 条 pattern 单测 + 规划无解日志 |
| 5 | Heapbait SLA 门控 probe → NotVulnerable | ⚠️ 部分 | 已实现 DA2 修补标记检查；`requires_sla` 无法静态验证，改为运行期记录（见 docs/heapbait_sla_gating.md） |
| 6 | seccfg unlock 后偏移 16 == 0 | ⏳ | 属 ops 阶段，本轮未改 |
| 7 | `--yes` 时警告仍出 stderr | ✅ 逻辑 | `DangerConfirm` 单测覆盖；CLI 接线随 ops 落地 |
| 8 | mock 下 DA 协议脱机跑 | ⚠️ 部分 | MockPort + fixture 已通，printgpt 闭环到 GPT 签名；完整协议状态机未接 |
| 9 | A/B 缓存 set_active 后读 boot 新槽 | ⏳ | 未做 |
| 10 | 新增日志走 LogKey | ✅ | 本轮无新增中文 log；plan 用户输出走 i18n |
| 11-15 | vbmeta 槽位/回退/混合布局 | ⏳ | 未做 |
| 16 | dumpbrom header 自描述 | ⏳ | 现有实现固定 128KB |
| 17 | `--mode` 约束 Planner | ✅ | `Mode::allows` + 单测 |
| 18 | `--log 1/2/3` 不影响决策日志 | ✅ 设计 | JSON 独立于 logger |
| 19 | `--log 3` hex dump + 搜索树 | ⏳ | 未做 |
| 20 | 每个 CVE 有结论 | ✅ | `docs/preloader_cve_status.md` |
| 21 | Preloader 只搜 Adapted | ✅ | 注册表无 Adapted → 规划报“无可用 Preloader exploit” |
| 22 | 主/兜底路径文档分开 | ⚠️ | preloader 文档已分开，BROM 文档待补 |
| 23 | adb-rs 回归 | — | 不在本仓库 |
| 24 | f2fs 生成器在 fs/ | ⏳ | 仍在 `system/fsimg/` |
| 25 | RPMB HACC/SSR 缺口明确 | ✅ | 实测为 SEJ，见 `docs/rpmb_sectors.md` |
| 26 | 芯片表含 V6 / Heapbait 缺口 | ✅ 部分 | `KNOWN_V6_DA_CODES` + docs |
| 27 | Cost 定义写进注释 | ✅ | `core/plan.rs` 模块注释 |

## 四、下一步（按依赖顺序）

1. **ops 迁移**：把 `cmd/partition_table::printgpt` 改成 `ops::printgpt`，走 `Orchestrator::decide` 后再执行，真正实现「唯一执行入口」。
2. **seccfg 偏移 16 修复** + 回读校验（验收 6）。
3. **vbmeta 槽位化**（验收 11-15）。
4. **fixture 接入真实协议状态机**：把 `da/xflash/protocol` 的 send/read 收口到 `StreamPort`，才能让 printgpt 全链路脱机跑（验收 8）。
5. **目录收口**：`system/fsimg → fs/`、`cmd/ops` 拆分、`slot/` 独立。

## 五、禁区遵守情况

- 未新增 exploit 解决“链少”；`heapbait`/`kamakiripl` 保持 `InProgress`，不进自动规划。
- `chain` 保留为调试视图，未作为必需命令。
- `printgpt` 未硬编码“先加载 DA”，V6 由 Planner 排除。
- `--yes` 不吞警告（`DangerConfirm` 返回 warning）。
- 未以 bkerler 原版为正确性标准；Carbonara 5 pattern、RPMB 用 SEJ 等结论均来自源码实测，并在 docs 记录差异。
- 未引入 clap 之外的重依赖；JSON 手写、bitset 自研。
