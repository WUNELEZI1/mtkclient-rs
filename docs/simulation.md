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
```

`--log 1`（默认）就能看到决策结论：

```
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(...) -> ... -> param_adjust(parse_gpt)
{"event":"decision","goal":"printgpt",...}
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
| `unplug` | 第 120 主机字节起断开 | 报"设备已断开" |
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
exploit=kamakiri2                  # 设备侧接受的 exploit（仿真专用）
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
| DA/XFlash | 包头 `FEEEEEEF` + `READ_DATA`(56B 参数) + `DEVICE_CTRL` + `EMMC_INFO` + `GET_DA_VER` + `CUSTOM_ACK` + `CUSTOM_SET_STORAGE` | 数据包 / 状态包分离；`data_type=2` 异步消息会被跳过 |
| 未覆盖 | 写 / 擦 / FORMAT / RPMB / XML(V6) 命令 | 仿真目前只服务读路径；写路径仍走 `DAXFlash` |

## 五、明确的局限（不要误读）

1. **exploit 不是真机复现**。kamakiri2 / carbonara 依赖 USB 控制传输时序与芯片地址表，
   纯字节流仿真是模拟不出来的。仿真里用一条设备侧命令 `0x1F <id>` 表示"设备接受该 exploit"，
   用于验证 **编排链路**（规划 → 执行 → 能力提升），不代表漏洞真的打成功了。
   代码里所有相关入口都标注了 `sim-only`。
2. **真实设备路径仍然走 `DAXFlash`**。`RealSession` 只把步骤映射到既有方法
   （`upload_da` / `read_gpt`），没有替换真机协议实现，因此不会引入未验证的回归。
3. **仿真设备不校验签名**。DA 字节只用于走完上传流程与协议版本判定。
4. 存储镜像只生成前 1 MiB（GPT 区域），越界读返回 0；`r` 分区读在仿真下不可用。

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
