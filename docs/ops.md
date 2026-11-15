# ops 层契约（读 / 写 / 擦 / seccfg / vbmeta / unlock / fs_shell）

`src/ops/` 里的每个操作只做两件事：**声明需要哪些能力**、**能力齐了之后做什么**。
"怎么拿到这些能力"（要不要先打 exploit、要不要先开存储）由 Planner 决定，`ops` 不关心。

```rust
pub trait Op {
    fn name(&self) -> &'static str;
    fn required_caps(&self) -> CapabilitySet;
    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpReport, String>;
}
```

## 一、能力声明一览

| op | 命令 | `required_caps` | 关键约束 |
|---|---|---|---|
| `read` | `r <part> <out>` | `StorageRead \| GptParsed` | 1 MiB 分块 → 写 `<out>.part` → 原子 rename |
| `write` | `w <part> <img>` | `StorageWrite \| GptParsed` | 大小校验**不截断**；重点分区确认；checkpoint；回读校验 |
| `erase` | `e <part>` | `StorageWrite \| GptParsed` | 重点分区确认；擦后抽样验证；幂等 |
| `seccfg` | `seccfg lock\|unlock` | `StorageRead\|StorageWrite\|GptParsed` | 写后回读断言 0x0C / 0x10 |
| `vbmeta` | `vbmeta [flags]` | `StorageWrite \| GptParsed` | 先定槽位，定不了就中止 |
| `unlock` | `unlock` | `StorageRead\|StorageWrite\|GptParsed` | seccfg unlock → vbmeta(flags=3) |
| `fs_shell` | `fs_shell boot\|proinfo <cmd>` | 只读 `StorageRead`；写入类额外 `StorageWrite` | 当前实现为只读视图 |
| `printgpt` | `printgpt` | `StorageRead \| GptParsed` | 产出 scatter.txt / gpt_debug.bin |

> 注意 `StorageWrite` 在能力偏序里蕴含 `StorageRead`（要写必先能读），
> 所以写类 op 的 `required_caps` 不必重复列 `StorageRead`。

## 二、危险确认策略

| 动作 | `Danger` 级别 | 确认方式 |
|---|---|---|
| 写/擦**重点分区** | `Irreversible` | 每次都要 token（设备 SN 后 4 位）或交互输入 token |
| seccfg lock/unlock、vbmeta 改写 | `High` | 同会话确认一次；`--yes` 可跳过交互 |
| 普通数据分区写/擦 | 无 | 不打断 |

- 重点分区判定见 `ops::is_critical_partition()`：preloader / seccfg / nvram / nvdata / persist /
  protect1-2 / proinfo / frp / misc / para / expdb / metadata / userdata / super / boot /
  init_boot / vendor_boot / recovery / lk / tee / dtbo / `vbmeta*` / `md1img` / `scp` / `sspm` / `spmfw` …
- **`--yes` 不等于静默**：警告仍打 stderr，并写一行 `[audit] <时间> action=... sn=... token=... fp=...`
  审计日志，事后可追溯。

## 三、写入路径的四道闸

1. **大小校验**：`image > 分区` 直接报错，**绝不截断**（截断写 = 悄悄破坏）。
2. **确认**：重点分区二次确认（见上）。
3. **checkpoint**：每 8 MiB 落一次盘，掉线后可续。
4. **回读校验**：写完回读全量比对；续传场景**强制校验**，校验不过则作废断点重写。

### checkpoint 文件格式（`<镜像路径>.resume`，纯文本 `key=value`）

```
version=1
part=boot_b
addr=0x2400000
total=20971520
written=8388608
file_size=20971520
file_mtime=1794719823
```

四项指纹（`part` / `addr` / `total` / `file_size`+`file_mtime`）任一不符就丢弃断点。
写入用"临时文件 + 原子 rename"，避免断电留下半个 checkpoint；**成功后删除**。

## 四、seccfg V4 的偏移语义（易错点）

| 偏移 | 字段 | unlock 写 | lock 写 |
|---|---|---|---|
| `0x0C` | `lock_state` | `3`（unlocked） | `1`（locked） |
| `0x10` | `dm_verity_state` | `0` | `0` |

上游 `critical=True` 时 unlock 会往 `0x10` 写 `1`（= `DM_VERITY_GENERAL_ERROR`），
这是为了让下次开机**验签失败进 dm-verity 错误态**而不是静默。本仓库保持 `0`：
在无真机对拍条件下，写 `1` 属于把设备推进错误态的高风险动作。该行为有测试锁死
（`seccfg_unlock_writes_dm_verity_zero`）。

## 五、vbmeta 槽位三级降级

```
bootctrl（misc 里读） → GPT 元数据里的 slot 后缀 → 报错中止（不猜）
```

- 槽位确定后，只处理**活动槽**的分区（`vbmeta` / `vbmeta_system` / `vbmeta_vendor` 及其 `_a`/`_b`）。
- 非活动槽或未找到的分区记 `[SKIP] <分区> : <原因>`，不写。
- 槽位不可判定时**一个字节都不写**，直接失败退出（`vbmeta_aborts_without_writing_when_slot_unknown`）。

## 六、擦除的幂等性

`e` 先 `FORMAT`（`CONTINUE → ack → COMPLETE` 三段），再抽样读前 4 KiB，断言全 `0xFF` 或全 `0x00`。
中断后重跑同一条命令不会报错、也不会把分区搞成"半擦"状态——擦除本身是幂等的，
非幂等的是"擦到一半又写了半截数据"，那属于写入路径，由 checkpoint 兜住。

## 七、扩展一个新 op 的成本

1. `src/ops/<name>.rs` 实现 `Op`（约 60~150 行）；
2. `src/ops/mod.rs` 里 `pub mod <name>;`；
3. `cmd` 层一行 `run_op(MyOp { .. })?`。

Planner / Executor / Session **都不用改**。能力声明由
`exec/ops_flow.rs::operations_declare_the_capabilities_they_use` 锁住（只读 op 不得声明写、
写 op 不得多要 RPMB/XML 这类无关能力）；声明漏了会在规划阶段直接暴露成
`gap=[storage_write]` —— 少声明拿不到能力，多声明会被测试拦住。
