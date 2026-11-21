# AGENTS.md — ZybClient / mtkclient-rs 项目大脑

> 本文件由项目全部历史记忆整合而成（`.workbuddy/memory/` 下 `MEMORY.md` + 2026-08-09～11-20 全部工作日志，以及交接包 `HANDOFF.md`）。
> 面向在此仓库工作的 agent：先遵守第 2 节铁律，再按第 3～6 节的环境事实与领域知识行动，避免重复踩坑。
> 记录日期：2026-11-20。带「最终结论」字样的条目是多轮反复后确认的状态，优先于流水账中的中间猜测。

---

## 1. 项目概览与当前状态

- **项目**：`mtkclient-rs`（工作目录 `D:\test\ZybClient`）——Rust 单 exe 的联发科（MTK）芯片刷机 / 取证工具。WinUSB 自动装驱动，`data/sdata.json` 动态加载芯片与 payload，支持读 / 写 / 擦分区、seccfg、AVB、fs_shell、断点续传、离线仿真。
- **平台**：x86_64（64 位），必须兼容 Win10 64 位。
- **当前分支 / HEAD**：`windows` 分支，HEAD = `42e6190`（2026-11-21，模块化拆分 + 注释英文化 + CI/CD + 发布加固；已 push `origin/windows`）。备份分支 `backup/pre-handoff-merge-20261120` 指向合并前的 `af52616`。
- **架构演进**：已从早期"能跑的工具"演进为 `core ← transport ← da ← exec/ops ← cmd` 的能力驱动架构（规划器 / 执行器 / 能力校验、全离线仿真、混沌故障注入、GPT CRC + 分块断点续传、写后 AVB 体检、carbonara-hardened、CVE-2022-20069 场景）。12 篇设计文档在 `docs/`。
- **阶段进度**：Phase 1 全部收口（1.1 混沌注入 / 1.2 CRC 断点续传 / 1.3 AVB 体检 + carbonara-hardened + CVE-2022-20069）；Phase 1.4 目录收口完成（`fs/`、`slot/`）；Phase 1.5 已完成 seccfg V3/V4 全版本解锁、ext4/f2fs 格式化接入、dead code 清零（`check`/`clippy --all-targets` 0 warning）。**剩余 Phase 1.5 候选**：设备透视 / 闪存健康 / 断点自愈（无细化规格）；之后才是 Phase 2 动态分区。
- **测试规模**：324 个单测 + 4 个黑盒集成测试全绿（仓库锁定 nightly，见 `rust-toolchain.toml`；最终二进制链接仍需用户本机）。

---

## 2. 不可违反的约定与铁律

### 2.1 用户长期约定（持续有效）
- **设备只读验证允许；严禁实际 flash / erase 写入。** 这是安全底线。
- 每次代码修改后必须 `git commit` + `git push`；优先新 commit，不用 `--amend`、不用 `--no-verify`。
- 输出语言中文，结尾给简洁总结，不用 emoji。
- 无硬编码：芯片 / payload 走 `data/sdata.json` 动态加载。**唯一允许的硬编码例外**：`CHIP_CONFIGS` 的 hw_code→寄存器地址表与设备状态表。
- 提交时不纳入：`.workbuddy/`、`data/`、`target/`、agent 文件、杂乱 / 测试文件；根目录的 `Log*.zip`、`Log.rar`、`log(4).7z`、`preload.so`、`*.bin` 等用户丢入的测试产物**禁止 git add**。

### 2.2 工程铁律（交接包继承）
1. **一次一 Phase、一步一 Commit** —— 不许攒大提交。
2. **仿真先行** —— 高危动作（写分区、擦除、改 seccfg）必须先在 `SimSession` 跑通测试，才允许接真机路径。
3. **绝对防御** —— 大小、哈希、校验任一不符立刻报错中止；严禁静默截断、严禁"猜一个值凑过去"。
4. **保持解耦** —— TUI 只调 Executor，不直接碰 transport。
5. **默认参数取最稳** —— 小白按回车不炸机，开发者用 flag 开激进模式；混沌注入默认全 0。
6. **查证优先** —— 用户给的 CVE 编号 / 地址 / 协议细节都要自己核对。案例：`CVE-2022-20435` 是误记，实为 **`CVE-2022-20069`**（Preloader OOB）；代码保留 `cve_20435_*` 别名但注释写明真实来源。

### 2.3 核心教训
- **"编译通过 ≠ 完美运行"，"自洽测试 ≠ 正确"。** 多个真实 bug 来自实现与测试共用同一错误假设：`boot_probe` make/parse 共用错误偏移、假 adbd 与实现共用大端假设、`create_minimal_ext4` 只测自己写的值——单测全绿但真机 / e2fsck 全错。真值以 AOSP 规范 / e2fsck / 真实源码（如 mtkclient Python）为准。
- **去死代码后至少跑 `cargo test --no-run`**：`#[cfg(test)]` 独占的方法（如 `TarArchiveCache::len()`）会被普通 build 误判为死代码删掉，导致测试编译断裂。

---

## 3. 环境与验证（沙箱限制，关键）

- **本沙箱缺少 MSVC 链接器**：唯一 `link.exe` 是 Git 自带的 GNU coreutils 版（`C:\Program Files\Git\usr\bin\link.exe`）；无 `msvcrt.lib`、无 `lld-link.exe`、无 MinGW gcc；rustup 只装了 `x86_64-pc-windows-msvc` / `i686-pc-windows-msvc` target。
- **`cargo build` / `cargo build --release` 会在链接步失败**（`link: extra operand` 或 UTF-16 BOM 错误），**沙箱无法产出可运行二进制**。环境会被重置，个别会话曾可用，但不要依赖。
- **`cargo check` 与 `cargo clippy --all-targets` 可以正常跑**（只类型检查 / 编译，不链接），这是沙箱内验证编译与告警的标配；`clippy --all-targets` 额外编译 test 目标，能暴露非 test 构建漏报的问题。`cargo fmt --check` 也可跑。
- **验证流程**：静态改动 → 沙箱 `cargo fmt --check` + `cargo check --all-targets` + `cargo clippy --all-targets -- -D warnings`（目标 0 error / 0 warning）→ 最终链接 / `cargo test` / 真机验证由用户在**本机**（PATH 含 MSVC 工具链）执行。
- 纯算法正确性（如自研 SHA-256）用 Python 以相同常量 / 逻辑对照 `hashlib` 交叉验证。
- **注释语言**：所有 Rust 源码注释（`//`、`///`、`//!`、块注释）一律英文；字符串字面量（日志、错误、i18n、help）保持现有语言。校验命令：`python scripts/check_cjk_comments.py`，结果必须为 0。
- **CI/CD**：`.github/workflows/ci.yml` 跑 nightly fmt/clippy/单测/集成测试/`cargo doc`；`release.yml` 产出 Windows x86_64 压缩包 + `SHA256SUMS.txt` + CycloneDX SBOM，并可选 Authenticode 签名（`scripts/sign.ps1`）。
- **shell 差异**：用户在 PowerShell（非 Git Bash）；`curl` 是 `Invoke-WebRequest` 别名，不支持 `-L -O`；Windows PowerShell 5.1 不认 `&&`。给命令前先区分 shell。
- **`core.autocrlf=true`**（项目当前设置，约定应为 false）：沙箱编辑写 LF、git add 存 LF，`git status` 会刷大量 "LF will be replaced by CRLF" 噪音。判定真实改动用 `git diff --ignore-all-space --stat`。改成 false 前需评估全仓 CRLF 重归一化的大 diff。
- 仓库 `.cargo/config.toml` 的 `[target.x86_64-pc-windows-msvc]` 含 `--remap-path-prefix` 去路径信息；链接期另可加 MSVC `/OPT:REF` + `/OPT:ICF`（纯减体积，不影响运行）。
- 交接包恢复需 Rust nightly（edition 2024，见 `rust-toolchain.toml`）、`libusb-1.0-dev`、`pkg-config`（真机 USB 路径需要，纯仿真不需要）、Python 3.11（测试辅助脚本）。无私有依赖、无网络构建依赖。

---

## 4. 构建配置（Cargo profile）

- **release 档以运行时性能最强为第一优先**（2026-08-25 由体积最小 `z` 改回）：`opt-level = 3` + `lto = "fat"` + `codegen-units = 1` + `strip = "symbols"` + `panic = "abort"` + `overflow-checks = false` + `debug = false` + `incremental = false`。用户诉求：编译慢无所谓，运行时最快、无调试符号。
- **dev 档追求最快编译**：`opt-level = 0` + `incremental = true` + `codegen-units = 256` + `lto = false`。
- **fast 档** inherits dev：`opt-level = 2` + `lto = "thin"`，日常平衡。

---

## 5. 依赖策略（v0.1.6 起，持续有效）

- **已自研移除的第三方 crate**（均有等价自研实现，clippy 0 warning，已 push）：
  `is_elevated` → `src/connection/driver/admin.rs` 裸 Advapi32 FFI；`ctrlc` → `src/cancel.rs` 裸 Kernel32 `SetConsoleCtrlHandler`；`colored` → `src/color.rs`（自研 Colorize trait + Win32 虚拟终端启用）；`memchr` → `src/util.rs::memmem_find`；`unicode-width` → `src/util.rs::display_width/is_fullwidth`；`env_logger` → `src/main.rs::TerminalLogger`（`log` 仅留作 trait 来源，需开 `std` 特性）；`indicatif` → `src/progress.rs`；`sha2` → `src/sha.rs`（FIPS 180-4，含回归向量）；另移除 `flate2`/`tar`（tar.gz 整体删除，改读 `data/`）、`tokio`/`anyhow`（零引用）。
- **绝对不动**：`aes` / `cbc`（密码学核心，自研错误 = 刷机损坏）、`nusb`（USB 通信核心）、`wdi-rs`（WinUSB 驱动安装）、`log`（轻量门面）。
- **保留决策**：`clap`（派生式 CLI 路由，自研风险高——静默误路由破坏性命令 = 变砖）、`serialport`（集成深，185 处 / 6 文件）。
- **剩余第三方依赖全集**：`log` / `clap` / `serialport` / `aes` / `cbc` / `nusb` / `wdi-rs`。
- **减体积已到底**：`clap` 用 `default-features=false, features=["std","derive","help"]`（去 color/suggestions/error-context/unicode/env/wrap_help；项目用自研 color.rs、CLI 为 ASCII、version 用标准 `env!` 宏）；`serialport` 在 Windows 上无 feature 可裁（唯一默认 feature `libudev` 是 cfg(linux) 专属）。
- **特性锁定（防御性）**：`nusb` / `serialport` 显式 `default-features = false`——`nusb` 不启用 smol/tokio 异步运行时（保持纯同步），`serialport` 不依赖 linux-only `libudev`。Windows 上无体积差异，价值在防止上游未来偷偷加默认特性。
- **坑**：`cargo clippy --fix`（含 `--broken-code`/`--bin`）会引入 E0502 借用冲突等错误，不可信；用受控手段改动。

---

## 6. 核心领域知识（MTK 协议 / 已踩坑）

### 6.1 芯片与资源加载
- Rust 后端按 **hw_code** 解析芯片（`CHIP_CONFIGS.iter().find(|c| c.hw_code == hw)`），与芯片"名字"无关；`ChipConfig.name` 仅用于日志显示。`sdata.json` **不被 Rust 消费**，是给外部 C# GUI（刷机匣 GeekFlashTool）做芯片选择 / payload 映射的。
- 同一 hw_code 多条目会被 `find` 首次命中吃掉——`chips_generated.rs` 保持合并条目（0x788→"MT6771/MT8385/MT8183/MT8666"，0x707→"MT6768/MT6769"）；显式命名落在 `sdata.json` 的 `support_chip` 与每芯片 `payloads` 键。
- `resolve_chip_payload` 同时匹配 hw_code（0x707）与 da_code（0x6768），归一化短键后查 sdata `payloads`，回退 `chip.loader`；最终经 `获取可执行文件相对路径` 解析到 `data/`。DA 本身统一走 `MTK_DA_V5.bin`。
- 目标设备家族 TB8788P1：MT6771/MT8385/MT8183/MT8666 共享 **hw_code 0x788**，单 DA 通吃。BROM 模式（0E8D:0003）末尾设备断开是连接稳定性问题；高通 dipper（SDM845/Firehose）不属本工具范畴。

### 6.2 preloader 提取（read32 扫描）— 最终结论
- 签名常位于 `0x00200000+0xF10`（偏移 3856），远离块首。扫描必须读 **512KB 窗口**（分 64KB 安全块，避免超 BROM 单次 0xD1 读取上限）并直接搜 8 字节签名 `SIG=[0x4D,0x4D,0x4D,0x01,0x38,0x00,0x00,0x00]`；不能只做 512B 快速预检，也不能只搜 `MTK_BLOADER_INFO` 字符串。
- **Kamakiri2 bypass 后 read32 本就能读 preloader RAM**（Mem Read Auth 已解除），无需再注入 dump payload；bypass 后二次注入 payload 会 `ack 超时`，不可用。
- **0x0 安全设备**（SBC/SLA/DAA/MemRead 全 False）：BROM 对 SRAM 的 read32(0xD1) 限制与这些标志无关，仍被拒。最终决策：自动提取 preloader（未给 `--preloader`）时窄范围强制 Kamakiri2 bypass，即 `needs_bypass = has_security || auto_extract`（后续修正还补了 `|| memwrite`，覆盖仅开 Mem Write Auth 的刁钻状态）。已指定 `--preloader` 的通用 DA 路径不受影响。
- **DA 模式快路径（更优）**：DA 已加载时用 XFlash `read_data` 一次读 512KB preloader（parttype=1、addr=0，校验 `EMMC_BOOT` 魔法，`BRLYT` 在 0x200），不走 Kamakiri BROM read32，更快更稳。需注意路由：DA 已加载时也要进 `dump::cmd_dumppreloader`，否则 DA 快读是死代码。

### 6.3 DA 会话与连接（最终结论）
- **MTK 设备 DA 会话不跨进程存活**——无论 BROM（PID=0x0003）还是 Preloader（PID=0x2000），进程退出即复位、DA 死。PID 恒定导致 `device_online()`（仅做 PID/VID 字符串比较）误命中，**`.state` 不得用于"连接复用 DA 会话"**。最终实现：所有模式统一 `smart_init` 重新握手 + 重载 DA；`.state` 仅保留缓存元数据（gpt_cache / optional_query_failures / preloader_path）。曾因此导致连续两条 `--mode preloader`/`--mode brom` 命令卡死（连已死 DA 进 reconnect_loop）。
- **握手规律（对齐刷机匣成功路径）**：Preloader VCOM(0x2000) 串口全程；设备握手时逐字节输出**完整两遍 READY**（R E A D Y ×2 = 10 字节）后才进入逐字节取反匹配（A0→5F, 0A→F5, 50→AF, 05→FA）。要点：READY 字母不计入 mismatch（max_mismatch=8 < 10 会误判）；字节 0 同步试探阶段容忍无响应、持续写 A0（SYNC_PROBE_LIMIT=25）；每次 attempt 前 drain 会吞 READY，仅 attempt>0 才 drain。USB bulk 介质可能原样回显（发 A0 收 A0），握手需同时容忍取反与回显两种模式。
- 串口握手失败时要回显最近收到的原始字节（如 0x52='R'）供诊断；握手期每字节超时缩至 500ms。
- 驱动：WinUSB 首次安装要稳——`switch_to_winusb` 整段安装（prepare→强制安装→重枚举→验证）包进 3 次重试、指数退避 800/1600/3200ms，装完用 `SetupDiRestartDevices` 主动重枚举。检测循环遇 `Unknown`（未绑驱动）设备要主动 `switch_to_winusb()`，否则别人机器上无驱动会卡死等待。
- 幽灵 COM 口：`scan_preloader_ports` 返回所有候选，逐个 `open_raw`，打不开就跳过试下一个，全失败才 sleep 重扫（15s 超时）。
- payload 注入 ack 读取加 3 次重试（仅 I/O 错误 / 字节不足时重试，magic 不匹配不重试，避免误重发已执行 payload）。
- RAM 读 preloader 的 `read_region_raw` 单 64KB 块加 3 次瞬态重试（每次重发 0xD1）。

### 6.4 reboot（多轮反复后的最终结论）
- **设计原则**：只有 `reboot --via preloader` 才纯 Preloader（DA 加载前拦截 → send_boot_pattern，纯 BromTransport）；其余所有 reboot（默认 system、fastboot/recovery/fastbootd/meta、`--via para/misc/da/xml`）一律 bypass→upload_da → DA 命令 / DA SHUTDOWN。
- **裸看门狗 vs DA SHUTDOWN**：裸硬件看门狗（`trigger_meta_reboot`，BROM WRITE32 watchdog+0x14=0x1209）在"无 DA 活跃"时能进系统，但"DA 活跃"时因 BROM echo 协议失效（"echo 0xD4 不匹配"）不可用。DA SHUTDOWN 是软跳转（DA 跳回 preloader），**必须在成功后 `close_device()` + 500ms 等电气重枚举**，否则主机挂着 USB、设备停在下载态（"日志成功但没进系统"）——对齐 mtkclient 的 `port.close(reset=True)`。
- **`da_shutdown` 参数（对照 mtkclient xflash_lib.py 权威值）**：命令码 `0x010007`（SHUTDOWN）；`enablewdt = 0`（禁用看门狗，重启到系统完全由 bootmode 控制）；32 字节参数顺序 `hasflags, enablewdt, async_mode, bootmode, dl_bit, dont_resetrtc, leaveusb, 0`（8×u32）。`ShutDownModes`：NORMAL=0（标准重启）、HOME_SCREEN=1（重启到系统）、FASTBOOT=2。system 走 `reset_device()` → `da_shutdown(HOME_SCREEN)`。
- **有未完成读取时拦截**：检测到 `<output>.resume` 且 `active_read=true`（Ctrl+C 续传态）时，reboot 不执行 DA SHUTDOWN，保持会话并告警列出待读分区，提示读完再重启；给删除 `.img`/`.resume` 强制重启的 escape hatch。
- `--via preloader` 在 DA 活跃时 Pattern 协议不可用，降级为 DA 默认重启并明确提示；`--mode preloader` 串口握手失败且 `.state` 标记 da_loaded 时，用有界探测复用活 DA 会话（`allow_da_fallback`），成功则走 DA 路径。
- DA 操作（如 `e expdb`）后设备仍停 DA 模式，此时 `--mode preloader reboot` 串口握手会失败——改用 `--mode auto`/`--mode brom`。
- BootAs Pattern 回传确认：累积所有读取字节到 `acc: Vec<u8>` 再窗口匹配 `TOOBTSAF`/`ATEM*`，不能对每个 ≤16 字节的小包单独匹配（分片会误报超时）。

### 6.5 读写路径 / 性能
- `rl`/`r` 断点续传：`compute_read_resume_offset` 统一"已完成跳过 + 512 对齐截断"逻辑；跨进程重跑要先删 `.resume` 活跃标记，重新发 READ_DATA 从断点续读（而非 ACK 续接旧流）。**续传偏移不得双重叠加**：`readflash_to_file(..., start_offset)` 内部会在非活跃分支下发 `addr + start_offset`，调用方必须传分区**基址**（曾传 `base+start_offset` 导致读分区外，数据损坏）。`size - start_offset` 要防下溢（start_offset>size 时报错）。
- **DA 读 flash 尾帧（最终结论）**：MT6768 DA 对**每个**读数据块都附加 8 字节尾帧（`DA_READ_PER_CHUNK_TRAILER=8`，状态/校验，被计入 slength）。每块按 `data_len = slen - 8` 截取并剥离尾帧；`slen==0` 视为终止包 break。曾因中间块连尾帧写入导致 8 字节/块错位、GPT 分区 CRC 失败、printgpt 读 0 分区。末态包是独立的 12 头+状态包。
- **速度根因（2026-11-20 修正，最终结论）**：**协议里不存在 `SET_PKT_LEN`**。mtkclient 权威命令表 `xflash_param.py` 中 `0x040008` 是 `GET_RANDOM_ID`，`readflash` 只调 `get_packet_length()` 不 SET。旧实现给 `0x040008` 带 4 字节参数发送，把 DA 回的 16 字节随机 ID 误认作 flow payload，破坏状态机，导致紧随其后的 `READ_DATA` 返回 `0xC0010003`（Unsupported command），正是真机 `printgpt` 失败根因；已在 commit `1ef6e6b` 删除读路径的 `set_read_packet_length`，读包长度以 DA 自报值为准、异常回退 1 MiB。**禁止再把 `0x040008` 当 SET_PKT_LEN 重新引入**（写路径 `partition/write/cmd.rs` 早已按同原理修正）。批量传输优化只能靠 BULK_CHUNK（已 64KB→2MB），不能靠伪造协议命令。
- **写速度**：写路径把 256KB 块经 `send_param_list_chunked` 按 `XFLASH_PARAM_CHUNK=0x200`(512B) 切片发送，32MB=65536 次 tiny USB 传输，导致写 ~1.74MB/s（读 ~7MB/s）。曾评估"批量数据改走 DA bulk data 大块 send_data"，用户选择暂不管。
- **写超时 / 流控**：WinUSB 后端 `is_libusb()` 返回 true，故 `clear_halt_out` 本就会调用。`write_with_retry` 每次重试先 `drain_pending()` 排空 IN 残留（解 DA 流控反压），末步再 `clear_halt_out()`+`clear_halt_in()`。`send_emi`/`boot_to` 也应走重试而非裸 write。
- **fast 路径死锁**：`recycle_tx.send` 在 `da.da_x_speed>=3`（默认）下主循环不消费 recycle 池，有界 SyncSender 满(128)后阻塞导致 writer/main 互锁——改 `try_send`。
- brom 比 preloader 慢的固有成本：WinUSB 驱动切换 + 重枚举 ~14s、bypass 后重握手 ~16s；日常读 GPT 用 `--mode preloader` 更快，brom 用于未烧 preloader / 砖机救砖。brom 路径曾做理论优化：DEFAULT_TIMEOUT_MS 5000→1500、盲等 sleep 改事件驱动 `drain_until_quiet` 等。

### 6.6 分区 / 镜像
- `scatter` 文件系统分区（system/vendor/userdata…）的 `type` 要写 `EXT4`（原全 `NORMAL_ROM`，SPFT 无法格式化写入）；`platform` 名 / boot 块 size 曾硬编码 MT6768 / 0x400000，应从 hw_code 推导（仍属待确认项）。
- `w gpt` / `recompute_gpt_crc`：写回 GPT 时重算分区项与头部 CRC。
- `rl`/`wl` 只导出 GPT 物理分区（含整块 super.img），未自动展开 super 内逻辑分区；单分区动态读写用 `r super --dp <name>`。
- `rl --skip` 曾在 `cmd_args`（尾部位置参数 allow_hyphen_values）中被吞掉，修法：同时扫描根级 `skip_partitions` 与 `cmd_args` 中的 `--skip <val>` / `--skip=<val>`。

---

## 7. 架构与代码组织（交接包快照）

- 分层：`core ← transport ← da ← exec/ops ← cmd`；新增 `core`/`exploit`/`exec`/`ops`/`session`/`transport` 等层，能力格与规划器 / 执行器 / 能力校验，全离线仿真（v5-clean/v5-sla/v6/flaky/unplug 等画像）+ 混沌故障注入。
- **≤500 行约束（用户硬要求，每个 .rs ≤500 行）**：13 个超 500 行文件已拆为目录模块（最大 478 行）。拆分清单：`explorer.rs→explorer/{mod,lp,ext4,shell,shell2,shell3,read}.rs`、`io.rs→io/{mod,resume,crc,gpt,read,write}.rs`、`super.rs→lp/{mod,types,parse}.rs`（源文件名 super.rs 但模块名 lp，目录必须叫 lp/）、`carbonara.rs→carbonara/{mod,stage,util}.rs`、`cmd/io.rs→io/{mod,read,write,erase,reboot,boot_mode,slot}.rs`、`cmd/mod.rs→根 + dispatch/handle/zyb/tests.rs`、`protocol.rs→protocol/{mod,ack,devctrl,read,shutdown,xml}.rs`、`da/xflash/io.rs→io/{mod,read,read_ex,write}.rs`、`transport.rs→transport/{mod,serial,usb}.rs`、`device_io.rs→device_io/{mod,read,write,control}.rs`、`brom_init.rs→brom_init/{mod,init}.rs`、`write.rs→write/{mod,stream,cmd}.rs`。
- **关键坑**：大量父模块用 `#[path = "X.rs"]` 声明子模块。把 `X.rs` 转成 `X/` 目录时必须同步删除 / 改写该 `#[path]`（改裸 `mod X;` 或 `#[path = "X"]`），否则编译失败。跨子模块私有辅助函数统一提为 `pub(crate)`（纯可见性调整，无逻辑变更）。
- 目录收口：`system/fsimg → fs/` ✅（2026-11-20，验收项 24 完成）；`fs/f2fs.rs` 715 行与 `fs/ext4.rs` 511 行已机械拆为 `fs/f2fs/{mod,layout}.rs`、`fs/ext4/{mod,tests}.rs`（全部 ≤500 行，内容等价校验通过）。**仓库仍有 18 个交接快照带来的超 500 行文件**（最大 `transport/vdev.rs` 2262 行、`i18n.rs` 1251 行、`system/chips_generated.rs` 1227 行），属快照合并后的新增债务，后续按风险分批拆。`slot/` 独立 ✅（2026-11-20，`partition/slot.rs → slot/mod.rs`，命令层 `cmd/io/slot.rs` 保留原位）；注意 slot 仍向下依赖 `partition::gpt::GptInfo`，partition 的 GPT 查找又回调 slot，属已知双向引用，未强行解耦以免改动 API。

---

## 8. 已知债务与遗留（未做 / 待确认）

- 真机回归未做（当前 Phase 1 验证基本在虚拟设备矩阵上）。
- RPMB / V6(XML) 仿真未实现。
- `dumpbrom` 输出缺头部自描述——因找不到可靠公开出处，选择"不猜"。
- 合并进来的快照模块仍有大量 dead_code 告警（cargo check 75 条；clippy bin 85 / test 55），均为尚未接线模块，为保持提交记录与快照一致未清理。
- 待确认的 review 项（WARNING 级，未盲动）：错误按字符串匹配分类脆弱、STALL 未 `clear_halt_in`、`sdata.rs` 配置损坏直接 `panic!` 宜改 `?`、`fs_shell` 短读 `unwrap` panic、scatter 平台名硬编码、`first.start_addr/2` 无意义回退、SET_PKT_LEN verify-after-set。
- 原始测试产物根目录 `Log (1).zip`/`Log (2).zip`/`Log (3).zip`/`Log.rar`/`log(4).7z`/`preload.so`/`*.bin`/`scatter.txt` 是用户资产，禁止删除 / 提交。

---

## 9. Git 与交付习惯

- 远端历史：早期私有库 `zyb-client`（322 提交）改名 `mtkclient-rs` 并公开；旧公开库改名 `mtkclient-rs-1` 保留发行版资产。标签 v0.1.0～v0.1.5 已打到历史提交。
- 2026-11-20 快照合并：浅克隆根提交 `af52616` 正好等于主线 `windows` 原 HEAD，主线 0 独有 / 快照 26 独有提交，`git merge --ff-only` 完整复刻（97 文件 +19862/-504），无冲突、无新提交。合并前建备份分支 `backup/pre-handoff-merge-20261120`。**用户明确选择暂不推送**，远端引用未动（无 Gitee 远端，`windows` 无 upstream；两个 GitHub 远端 origin/github、master/windows 分支均未改）。
- 交接包 `temp123/`：`workspace/`（已合并内容，约 513.6MB）已删除；保留 `temp123/meta/`（会话记录 / 任务快照 / 索引，33.75MB）与 `temp123/HANDOFF.md`。原始 `D:\Download\mtk完整交接包-20261115.tar.gz` 保留未删，可随时重新解压恢复。
- 交接包恢复注意：bundle 因 shallow clone 缺父对象**不能**独立 `git clone` 恢复；可靠路径是 `PR_patches/` 的 patch 或完整 tar 快照。

---

## 10. 关键 commit 索引（历史溯源，非当前 HEAD）

- 早期：`4a56b23`(动态加载 payload)、`964090a`(v0.1.5 da_x.bin 运行时加载)、`a88fe03`(双语 README)、`af6aefc`(会话重置判定 is_transport_error)、`5516fd6`(rl 断点续传)、`2912e27`/`a49d369`(续传双重偏移修复)。
- 重启链：`e83cf19`→`d2e5681`→`0f56a9a`→`bfc942c`→`bce046b`→`ef1e21a`→`2c84a72`→`e039752`（最终 08-18 19:40 用户确认闭环）。
- 握手：`6e51dbf`(open_raw+USB 回显兼容)、`a4680f3`(READY 两遍容忍)、`155be4e`(0x0 强制 bypass)。
- 依赖自研：`7b699ec`(color)、`f8af187`(is_elevated/ctrlc)、`4de7b94`(unicode-width)、`f079907`(env_logger)、`ac844a0`(indicatif)、`d914823`(sha2)、`ad01afe`(去 nusb tokio feature)。
- 拆分与读路径：`ed2fed0`(拆分后编译修复)、`03071dd`(尾帧初修)、`60a7881`(brom 写超时)、`3f5ab67`(多块尾帧错位 + 握手窗口)、`8d43bb3`(brom 性能优化)。
- Phase 1（快照内）：`6f3d2bd`(1.2 CRC+断点)、`aca059e`(1.3 AVB 体检)、`c2219fd`(carbonara-hardened)、`8d6be40`(CVE-2022-20069)。
- Phase 1.4/1.5：`c944f7e`(fs 目录收口)、`a102b48`(f2fs/ext4 拆分)、`b8e8798`(slot 独立)、`1ef6e6b`(printgpt 删 SET_PKT_LEN)、`9bdc86e`(seccfg V3/V4)、`b58165a`(ext4/f2fs 格式化接线)、`a248d76`/`c2f7673`(dead code 清零，当前 HEAD)。
