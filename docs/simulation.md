# 全离线仿真测试路径（`--sim` / `--port mock://`）

目标：**没有 MTK 设备也能跑完整链路**——`连接 → 芯片识别 → DA 上传 → 开存储 → 读 GPT → 打印分区表`，
并且每一步都真实经过协议状态机，而不是"喂一段写死的字节给解析器"。

```
CLI (--sim v5-clean printgpt)
  └─ Executor                     唯一执行入口
       ├─ Orchestrator.decide()   Planner 搜索 + 决策日志（gap / selected / alternatives）
       └─ 按 PlanStep 逐步调用 Session
            ├─ RealSession  →  DAXFlash（真机，USB/串口，行为不变）
            └─ SimSession   →  DaEngine（StreamPort 状态机）+ VirtualDevice（设备侧状态机）
```

## 一、命令

```bash
# 内置预设
mtkclient-rs --sim v5-clean printgpt
mtkclient-rs --sim v5-sla   printgpt      # SLA 打开：规划器必须先规划 exploit
mtkclient-rs --sim v6       printgpt      # V6：BROM 不可达，禁止规划 BROM 路径
mtkclient-rs --sim ufs-ab   printgpt      # UFS + A/B 布局
mtkclient-rs --sim flaky    printgpt      # 第 400 字节起设备静默 → 超时错误
mtkclient-rs --sim unplug   printgpt      # 第 120 字节起断开 → Disconnected 错误

# 等价写法（--port 是 --sim 的别名）
mtkclient-rs --port mock://v5-clean printgpt

# 自定义画像
mtkclient-rs --sim ./profiles/mt6765.sim printgpt

# 看完整协议流量（hex dump）与规划搜索树
mtkclient-rs --sim v5-clean --log 3 printgpt

# 读写擦闭环（全部走 Executor → Planner → SimSession，不经过真机代码）
mtkclient-rs --sim v5-clean r boot_a out.img
mtkclient-rs --sim v5-clean w boot_a boot.img --yes     # 写完自动回读校验
mtkclient-rs --sim v5-clean e userdata --yes            # 擦后抽样断言 0xFF/0x00

# 高危操作（写 seccfg / vbmeta 一律需要确认；交互式会先提示风险再问 y/N，加 --yes 则直接放行）
mtkclient-rs --sim v5-clean seccfg unlock --yes
mtkclient-rs --sim ufs-ab   vbmeta 3 --yes              # 只改活动槽，非活动槽打 [SKIP]
mtkclient-rs --sim v5-clean unlock --yes                # seccfg unlock + vbmeta 一条链

# 只读文件系统视图
mtkclient-rs --sim v5-clean fs_shell boot ls /
```

### 跨进程断点续传（仿真"拔插重连"）

`--sim-state <FILE>` 把虚拟设备的 flash 内容（稀疏页 + 擦除区间）落盘，
下一次进程用同一个文件即可继续——这正是真机"线上掉线 → 重新插上 → 从断点续写"的等价仿真。

```bash
# 第一跑：flaky-write 画像在 10 MiB 处硬断开
mtkclient-rs --sim flaky-write --sim-state /tmp/sim.state w boot_b /tmp/big.img --yes
#   → 设备已断开；.resume 落盘 written=0x800000（最后一次 8 MiB 周期检查点）

# 第二跑：换干净画像 + 同一份设备状态 = 重连
mtkclient-rs --sim v5-clean --sim-state /tmp/sim.state w boot_b /tmp/big.img --yes
#   → 断点续传: boot_b 已写 8388608/20971520 字节（40.0%），从 0x800000 继续
#   → 校验通过: 20971520 字节一致；成功后 .resume 自动删除
```

`--log 1`（默认）就能看到决策结论：

```
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(...) -> ... -> param_adjust(parse_gpt)
```

`--log 2` 起额外输出一行结构化决策日志（单行 JSON，键顺序固定，便于上报/解析）：

```json
{"event":"decision","goal":"printgpt","mode":"auto","device":"...","required":["storage_read","gpt_parsed"],"current":[],"gap":["storage_read","gpt_parsed"],"da_source":"sim-injected","selected":{"steps":["da_load(V5(XFlash))","param_adjust(open_storage)","param_adjust(parse_gpt)"],"cost":690,"reason":"..."},"alternatives":[{"steps":[...],"cost":840}]}
```

`--log 3` 额外给出：

| 来源 | 内容 |
|---|---|
| `da::engine` | 每次端口读写的带偏移 hex dump（`[engine] -> / <-`） |
| `exploit::planner` | BFS 每层 frontier/solutions + **每条被排除分支及原因** |
| `exec` | 每步带来的能力增量（`[exec] 能力增量: [da_loaded,da_xflash]`） |

## 二、预设画像

| 预设 | 场景 | 期望行为 |
|---|---|---|
| `v5-clean` | 干净 V5 机（无安全标志） | 3 步闭环，cost=690 |
| `v5-sla` | target_config `0x2`（SLA），`SEND_DA` 回 `0x1D0D` | 规划器先排 `exploit(kamakiri2)`，绕过后才允许上传 |
| `v5-daa` | SLA + DAA（`0x6`） | 同上，并验证安全位解析 |
| `v6` | V6(XML) 平台（MT6983） | `brom_reachable=false`，不生成 BROM 边；DA 段标 `V6(XML)` |
| `ufs-ab` | UFS、block=4096、A/B 分区 | 存储信息显示 UFS；分区表含 `_a` 槽 |
| `flaky` | 第 400 主机字节起静默 | `da_load` 报传输超时（步骤级错误，含设备事件） |
| `flaky-write` | 写入进行到 10 MiB 时设备硬断开 | `WriteDead` → 保留 `.resume`，重连后续传 |
| `unplug` | 第 120 主机字节起断开 | 报"设备已断开" |
| `ab-unknown-slot` | A/B 混合布局且 bootctrl 不可读 | `vbmeta` 必须报错中止，**不猜槽位** |
| `no-emmc-info` | 不支持 eMMC 信息查询 | 查询返回不支持状态，走降级路径 |

## 三、自定义画像格式

`key=value` 每行一条，`#` 开头为注释；`partition=` / `fault=` / `exploit=` 可重复。

```ini
name=MT6765
hw_code=0x0707
hw_sub=0x8A00
da_code=0x6765
proto=v5                 # v5 / v6
safety=0x6               # 0x1 SBC | 0x2 SLA | 0x4 DAA | 0x10 CERT | 0x20/0x40 mem
sla=true                 # SEND_DA 是否返回 0x1D0D
brom_auth=false
preloader_patched=false
da_sig_failed=false
emmc_info=true
storage.type=emmc        # emmc / ufs / sd / mmc / ufs_card
storage.block=512
storage.boot1=0x400000
storage.boot2=0x400000
storage.user=0x100000000
partition=boot_a:34:0x2000000      # 名称:起始LBA:字节数
partition=super:0x4000:0x80000000
fault=silent:400                   # silent（静默→超时）/ dead（断开）
fault=write_dead:0xA00000          # 写到该字节数时硬断开（断点续传用）
fault=write_silent:0xA00000        # 写到该字节数时静默（→ 超时）
exploit=kamakiri2                  # 设备侧接受的 exploit（仿真专用）
slot=a                             # 活动槽：a / b / none（none = 不可判定）
bootctrl=true                      # misc 里是否有可读 bootctrl
seccfg.lock_state=1                # 0x0C：1=locked，3=unlocked
seccfg.dm_verity=0                 # 0x10：0=正常，1=DM_VERITY_GENERAL_ERROR
```

未写 `partition=` 时使用默认 8 分区布局。`storage.user` 会自动放大到覆盖最后一个分区
（否则 GPT 里会出现超出盘尾的分区）。

## 四、协议覆盖范围

仿真设备与 `DaEngine` 共用 `transport::wire` 里的常量，并与既有 `da::xflash::protocol`
有**一致性测试**（`wire::tests::constants_match_legacy_protocol_layer`），所以"仿真通过"
不会退化成"另一套自造协议"。

| 层 | 已实现 | 说明 |
|---|---|---|
| BROM | 握手 `A0 0A 50 05`（逐字节取反）、`FE` sync、`FD` hw code、`D8` target config、`D7` SEND_DA、`D5` JUMP_DA | 命令支持 1 字节与 4 字节字两种编码（真机两种写法都存在） |
| DA/XFlash | 包头 `FEEEEEEF` + `READ_DATA`(56B 参数) + **`WRITE_DATA`(0x010004)** + **`FORMAT`(0x010003)** + **`CC_OPTIONAL_DOWNLOAD_ACT`(0x080005)** + `DEVICE_CTRL` + `EMMC_INFO` + `GET_DA_VER` + `CUSTOM_ACK` + `CUSTOM_SET_STORAGE` | 数据包 / 状态包分离；`data_type=2` 异步消息会被跳过；写参数块 56B（含 32B NandExtension），分块 `[0u32][checksum][data]` 按 0x200 切分 |
| 存储行为 | 稀疏 4 KiB 页 + 擦除区间合并 | 写页自动读-改-写；擦 1 GiB 只留几条区间记录，不会物化 26 万页 |
| 未覆盖 | RPMB / XML(V6) 命令 | RPMB 真机实测为 SEJ（见 `docs/rpmb_sectors.md`）；V6 XML 平台无 BROM 路径 |

## 五、明确的局限（不要误读）

1. **exploit 不是真机复现**。kamakiri2 / carbonara 依赖 USB 控制传输时序与芯片地址表，
   纯字节流仿真是模拟不出来的。仿真里用一条设备侧命令 `0x1F <id>` 表示"设备接受该 exploit"，
   用于验证 **编排链路**（规划 → 执行 → 能力提升），不代表漏洞真的打成功了。
   代码里所有相关入口都标注了 `sim-only`。
2. **真实设备路径仍然走 `DAXFlash`**。`RealSession` 只把步骤映射到既有方法
   （`upload_da` / `read_gpt`），没有替换真机协议实现，因此不会引入未验证的回归。
3. **仿真设备不校验签名**。DA 字节只用于走完上传流程与协议版本判定。
4. 存储镜像只生成前 1 MiB（GPT + 种子分区内容），**其余区域是稀疏的**：未写过的地方读 0。
   `r` / `w` / `e` 在仿真下可用，但只对 GPT 覆盖得到的地址范围有意义。
5. **写/擦的分块大小以 DA 自报为准**。仿真设备会回报和真机同量级的包长；`MTK_FORCE_PKT_LEN`
   是显式覆盖开关（历史上误把 `0x040008` 当 SET_PKT_LEN，实为 `GET_RANDOM_ID`，已修正）。
6. **断点续传的指纹包含文件大小与 mtime**。源镜像被改过则断点自动作废（宁可重写，不写错位数据）。

## 六、回归清单

`cargo test` 中与仿真相关的用例：

| 用例 | 覆盖点 |
|---|---|
| `transport::vdev::tests::*` | 握手 / 芯片识别 / GPT CRC / 画像解析 / 故障注入 |
| `da::engine::tests::*` | 连接 / 全流程到 GPT 签名 / SLA 拒绝 / exploit 解锁 / 存储信息 / 阶段守卫 |
| `exec::tests::printgpt_closes_the_loop_on_clean_device` | 端到端闭环 + cost=690 + 能力增量 + 分区数 |
| `exec::tests::sla_device_forces_exploit_first` | 安全标志 → 必须先 exploit |
| `exec::tests::v6_never_uses_brom_exploit` | V6 不生成 BROM 边 |
| `exec::tests::transport_fault_surfaces_as_step_error` | 故障 → 步骤级错误（不 panic、不静默） |
| `exec::tests::plan_failure_is_reported_without_panic` | 无 DA → 规划失败而非崩溃 |
| `exec::ops_flow::tests::*`（11 个） | r/w/e/seccfg/vbmeta/unlock 端到端 + 能力声明 |
| `exec::ops_flow::tests::flaky_write_resumes_from_checkpoint_after_replug` | 断点续传：`flaky-write` → replug → 从 0x800000 续写 |
| `exec::ops_flow::tests::erase_is_idempotent_and_blanks_partition` | 擦除中断后重来不出错 |
| `exec::ops_flow::tests::seccfg_unlock_writes_dm_verity_zero` | 断言 0x10 == 0x00 |
| `exec::ops_flow::tests::vbmeta_aborts_without_writing_when_slot_unknown` | 槽位未知 → 报错，不猜 |
| `transport::vdev::tests::state_roundtrip_preserves_pages_and_erased` | `--sim-state` 读写布局对称（页 + 擦除区间逐字节一致） |
| `transport::vdev::tests::state_load_rejects_bad_magic_and_version` | 损坏/旧版本状态文件被拒，不静默错乱 |

## 七、混沌仿真（`--sim-chaos`，Phase 1.1）

为了在没有真机的情况下逼出"抗物理干扰"缺陷，虚拟设备支持**确定性混沌注入**。
默认关闭（默认最稳）；显式加 `--sim-chaos` 才启用，用 `--sim-chaos-seed N` 固定故障序列以便复现。

```text
mtkclient-rs --sim v5-clean printgpt --sim-chaos --sim-chaos-seed 1
```

四类注入（概率均为千分比，见 `ChaosConfig`）：

| 注入 | 位置 | 语义 | 对应真机故障 |
|---|---|---|---|
| 位翻转 `bitflip` | 写入**校验和通过之后**落盘 | 落盘数据被改坏，只能靠回读校验/CRC 发现 | 闪存位衰变 |
| 丢包 `drop` | 读回数据返回前 | 尾部若干字节被丢弃（短包），主机拿到更少字节 | 劣质线材 |
| 静默 `silence` | 读 | 立即上报**断开**并锁存，直到 `replug` 才恢复 | 线缆脱落 |
| 瞬时超时 `glitch` | 读 | 单次读超时后**自动恢复**（不锁存） | 接触不良抖动 |

确定性：`SplitMix64(seed)`，同一种子 → 同一条故障序列（可复现回归）。

新增测试（`transport::vdev::tests::chaos_*`）：PRNG 确定性、位翻转原语、
静默锁存与重连恢复、瞬时超时不锁存、丢包截断且尾部真丢、关闭时行为完全一致、
以及"位翻转在 checksum 通过后才发生、回读必不一致"的端到端用例。

### 7.1 有界重试与续传硬化（Phase 1.1b）

1.1 的混沌冒烟暴露了真实缺陷：`--sim-chaos --sim-chaos-seed 7` 在 `da_load`
直接报"传输超时"。根因是底层读路径没有任何容错。1.1b 按"分层重试"修好：

| 层级 | 语义 | 触发条件 |
|---|---|---|
| 字节级（`RetryingPort::read_exact`） | **续读式**重试：保留已读进度，只补读剩余字节 | 连续无进展的 `Timeout`，上限 `--max-retries`（默认 3），指数退避 10/20/40…封顶 100ms |
| 命令级（`DaEngine::read_flash`） | 整条 `READ_DATA` 重发：先 `drain()` 清空输入缓冲，再重新同步 | 包被截断 / `magic` 错位 / 长度不符 / `Timeout` |

关键设计（三条铁律）：

- **断开绝不重试**：`Disconnected`（含静默锁存）立刻失败，交给上层触发断点续传 / 提示重插。
  静默在仿真中直接上报 `Disconnected`——因为它等价于"线缆脱落，需要人工重插"，
  当作可重试超时只会在死设备上白等。
- **写永不重试**：写不是幂等操作，重发会污染设备侧状态。重试只作用于读方向。
- **丢包只打数据块**：`drop` 仅对 ≥16 字节的读生效。真实 USB 短包不会丢尾部字节，
  控制帧（握手 / 回显 / 状态字）丢字节只会让主机无限等待，与物理现实不符。

验证：

```text
mtkclient-rs --sim v5-clean printgpt --sim-chaos --sim-chaos-seed 7    # 通过
mtkclient-rs --sim v5-clean printgpt --sim-chaos --sim-chaos-seed 42   # 通过
```

对应测试：`retry_recovers_transient_timeouts`、`retry_is_bounded_and_reports_timeout`、
`retry_resumes_after_partial_read`、`partial_read_then_persistent_timeout_fails`、
`disconnected_is_never_retried`、`none_policy_never_retries`、`write_is_never_retried`、
`backoff_is_exponential_and_capped`、`chaos_glitch_is_absorbed_by_bounded_retry`、
`chaos_drop_recovers_via_read_command_retry`、`chaos_silence_fails_fast_without_retry`。
