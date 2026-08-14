//! 用户命令入口
//!
//! 子模块：
//! - `cli`            — 命令行参数解析
//! - `dump`           — dumppreloader 等镜像提取
//! - `io`             — r/w/e/reboot/slot 等 IO 命令
//! - `partition_table` — printgpt / rl / wl 等分区表相关
//!
//! 顶层入口：
//! - `print_help`       — 打印帮助信息
//! - `handle_command`   — 单命令执行入口（处理 Phase1 dump/bypass + Phase2 DA 命令）

pub mod buildprop;
pub mod cli;
pub mod detect;
pub mod dump;
// 已移除: fastboot 改为独立二进制 fastboot-rs
pub mod io;
pub mod multi;
pub mod partition_table;
pub mod preloader_boot_mode;
pub mod script;

use colored::Colorize;
use log::{debug, error, info, warn};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;
use crate::usb::USB上下文;

pub fn print_help() {
    use colored::Colorize;

    println!("{}", "mtkclient-rs — MTK 设备底层刷机工具".bold());
    println!(
        "{} {} | {}",
        "版本:".dimmed(),
        env!("CARGO_PKG_VERSION").yellow(),
        "支持 87 种 MediaTek 芯片".green()
    );
    println!(
        "{} {}",
        "系统要求:".dimmed(),
        "Windows 10/11 64-bit | 管理员权限".cyan()
    );
    println!();
    println!("{}", "用法:".bold());
    println!("  mtkclient-rs.exe [全局选项] <命令> [参数...]");
    println!();
    println!("{}", "全局选项:".bold());
    println!("  --mode <mode>         工作模式: brom（默认，完整BROM流程+Kamakiri2 bypass）");
    println!("                                preloader（直接连接Preloader VCOM，跳过bypass）");
    println!("  --preloader <file>    指定 preloader 文件（默认从设备dump）");
    println!("  --da <file>           指定 DA loader 文件（默认 MTK_DA_V5.bin）");
    println!("  --da2 <file>          指定 DA2 文件");
    println!("  --da_x_speed <1-3>    DA 加载速度: 1=完整 2=快速 3=极速+USB高速重连");
    println!("  --log <level>         日志级别: 1=INFO 2=DEBUG 3=TRACE（含原始hex dump）");
    println!("  --quiet               静默模式（跳过info输出）");
    println!("  --quiet_dump          静默dump（不打印进度条）");
    println!("  --usb_log             启用 USB 通信追踪日志（输出到 usb_debug.log）");
    println!("  --no_elevate          跳过管理员权限检查");
    println!();
    println!("{}", "分区操作:".bold());
    println!("  printgpt                     打印 GPT 分区表 + eMMC 信息 + super动态分区树");
    println!(
        "  r <分区名> <文件|目录>         读取分区到文件（自动识别动态分区；目录则写入 <目录>/<分区名>.img）"
    );
    println!("       r boot boot.img          读取 boot 分区");
    println!("       r boot1 boot1.bin        读取 eMMC boot1（特殊分区）");
    println!("       r system system.img      自动从 super 读取动态分区");
    println!("       r super --dp system_b system_b.img   读取 super 内动态分区（指定槽位）");
    println!("       r gpt <目录>              读取 GPT 原始数据到目录");
    println!("  rl <目录> [--skip <分区,...>]  读取全部分区到目录");
    println!("  w <分区名> <文件>             写入文件到分区（自动识别动态分区）");
    println!("       w super super.img          直接写入 super 分区");
    println!("       w system system.img        自动写入 super 内的动态分区");
    println!("  wl <目录>                     从目录恢复全部分区（匹配 .bin/.img）");
    println!("  e <分区名>                    擦除分区");
    println!();
    println!("{}", "信息查询:".bold());
    println!("  zyb get_build_prop [分区名]   读取 build.prop（默认 system 分区）");
    println!("  slot show/a/b                 显示/切换 A/B 槽位");
    println!();
    println!("{}", "安全与解锁:".bold());
    println!("  zyb seccfg unlock             解锁 Bootloader（seccfg V3/V4 + 自动禁用 vbmeta）");
    println!("  zyb seccfg lock               锁定 Bootloader（seccfg V3/V4）");
    println!("  zyb oem unlock                FRP OEM 解锁（修改 frp 分区标志位）");
    println!("  zyb oem lock                  FRP OEM 回锁（修改 frp 分区标志位）");
    println!("  zyb vbmeta <mode>             修补 vbmeta（0=关闭验证 1=启用 2/3=保留）");
    println!("  zyb erase_data                清除用户数据 + metadata（恢复出厂设置）");
    println!("  frp                           FRP OEM 解锁（zyb oem unlock 的别名）");
    println!();
    println!("{}", "重启控制:".bold());
    println!("  reboot                        正常重启到系统");
    println!("  reboot fastboot               重启到 Bootloader（lk fastboot，仅 BROM 模式）");
    println!("  reboot recovery               重启到 Recovery");
    println!("  reboot fastbootd              重启到 fastbootd（userspace fastboot）");
    println!("  reboot meta                   重启到 META 模式");
    println!("  reboot <mode> --via misc      通过 misc 分区设置 bootloader_message（默认）");
    println!("  reboot <mode> --via para      通过 para 分区设置 boot_mode");
    println!("  reboot <mode> --via da        通过 DA CMD_RESET 命令重启（需先写 misc/para）");
    println!("  reboot <mode> --via xml       通过 XML DA SET-BOOT-MODE 重启（新平台）");
    println!("  reboot fastboot --via preloader 通过 Preloader Pattern 协议重启到 Bootloader");
    println!();
    println!("{}", "其他:".bold());
    println!("  dumppreloader                 从 RAM 提取 Preloader（BROM Exploit）");
    println!("  adb                           在 DA 模式下开启 ADB 并重启");
    println!("  peek <addr> [size]            读取设备内存（默认 4 字节），输出 hex dump");
    println!("  poke <addr> <hex_data>        写入数据到设备内存（hex 格式）");
    println!("  fs_shell                      交互式浏览 super 分区文件系统");
    println!("  multi \"<cmd1>;<cmd2>;...\"    一次 DA 会话执行多个命令");
    println!("  run <script.json>             从 JSON 脚本文件批量执行命令");
    println!();
    println!("{}", "快速开始示例:".dimmed().bold());
    println!(
        "  {}                                    查看设备分区表",
        "mtkclient-rs printgpt".cyan()
    );
    println!(
        "  {}                     备份 boot 分区",
        "mtkclient-rs r boot boot.img".cyan()
    );
    println!(
        "  {}              解锁 Bootloader（自动禁用 dm-verity）",
        "mtkclient-rs zyb seccfg unlock".cyan()
    );
    println!(
        "  {}                          恢复出厂设置",
        "mtkclient-rs zyb erase_data".cyan()
    );
    println!(
        "  {}                      刷入修改后的 boot",
        "mtkclient-rs w boot magisk_patched.img".cyan()
    );
    println!(
        "  {}                    重启到 fastboot 模式",
        "mtkclient-rs reboot fastboot".cyan()
    );
    println!();
    println!("{}", "开发示例 (cargo):".dimmed());
    println!("  cargo run -- --mode brom printgpt");
    println!("  cargo run -- --mode brom r boot_a boot_a.img");
    println!("  cargo run -- --mode brom zyb seccfg unlock");
    println!("  cargo run -- --mode preloader reboot fastboot");
    println!();
    println!("{}", "作者声明:".dimmed().bold());
    println!("  {}  无能乐子(wunelezi)", "作者:".dimmed());
    println!(
        "  {}  https://gitee.com/WUNELEZI1/mtkclient-rs/releases",
        "更新:".dimmed()
    );
    println!("  {}    3535571067", "QQ:".dimmed());
    println!(
        "  {}",
        "本工具完全免费，请勿被骗！禁止倒卖、逆向破解或去除作者信息。"
            .red()
            .dimmed()
    );
    println!(
        "  {}",
        "基于 Apache License V2 协议开源，衍生作品须保留原作者署名。".dimmed()
    );
    println!();
    println!("{}", "致谢与参考:".dimmed().bold());
    println!(
        "  {}  mtkclient by bkerler — {}",
        "协议参考:".dimmed(),
        "https://github.com/bkerler/mtkclient".blue()
    );
    println!("  {}  刷机匣 GeekFlashTool (C# 实现)", "工具参考:".dimmed());
    println!(
        "  {}  payload / DA / EMI 等资源文件来自 mtkclient 项目 (bkerler)",
        "资源归属:".dimmed()
    );
    println!(
        "  {}",
        "本项目代码为独立 Rust 实现，与上述项目无代码复用关系。".dimmed()
    );
}

/// 预检查命令有效性（在 DA 加载之前）
/// 返回 Ok(()) 表示命令格式正确，Err 表示命令不存在或参数不合法
fn validate_command(cmd: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        "printgpt" | "dumppreloader" | "adb" | "fs_shell" | "frp" => {}
        "peek" => {
            if args.is_empty() {
                return Err("用法: peek <addr> [size]".into());
            }
            // 提前校验地址和 size，避免加载 DA 后才发现参数错误
            let _addr = io::parse_addr(&args[0])?;
            if args.len() >= 2 {
                let size = io::parse_size(&args[1])?;
                if size == 0 {
                    return Err("size 不能为 0".into());
                }
                if size > 0x100000 {
                    return Err("size 不能超过 1MB（peek 适用于小范围内存读取）".into());
                }
            }
        }
        "poke" => {
            if args.len() < 2 {
                return Err("用法: poke <addr> <hex_data>".into());
            }
            let _addr = io::parse_addr(&args[0])?;
            let _data =
                crate::util::parse_hex(&args[1]).map_err(|e| format!("hex 数据解析失败: {}", e))?;
        }
        "r" => {
            if args.is_empty() {
                return Err("用法: mtkclient r <分区名> <文件>".into());
            }
        }
        "rl" => {
            if args.is_empty() {
                return Err("用法: mtkclient rl <目录>".into());
            }
        }
        "w" => {
            if args.len() < 2 {
                return Err("用法: mtkclient w <分区名> <文件>".into());
            }
        }
        "wl" => {
            if args.is_empty() {
                return Err("用法: mtkclient wl <目录>".into());
            }
        }
        "e" => {
            if args.is_empty() {
                return Err("用法: mtkclient e <分区名>".into());
            }
        }
        "reboot" => {}
        "slot" => {
            if args.is_empty() {
                return Err("用法: mtkclient slot show|a|b".into());
            }
            // 提前校验槽位子操作，命令拼错时直接返回用法提示，
            // 不加载 DA、也不触发会话重置（对齐用户: 仅命令错了就告诉用户怎么用）
            match args[0].as_str() {
                "show" | "a" | "b" => {}
                other => {
                    return Err(
                        format!("用法: mtkclient slot show|a|b（未知槽位操作: {}）", other).into(),
                    );
                }
            }
        }
        "zyb" => {
            if args.is_empty() {
                return Err("用法: mtkclient zyb <subcmd> [args]".into());
            }
            match args[0].as_str() {
                "seccfg" => {
                    if args.len() < 2 || !matches!(args[1].as_str(), "unlock" | "lock") {
                        return Err("用法: mtkclient zyb seccfg unlock|lock".into());
                    }
                }
                "oem" => {
                    if args.len() < 2 || !matches!(args[1].as_str(), "unlock" | "lock") {
                        return Err("用法: mtkclient zyb oem unlock|lock".into());
                    }
                }
                "vbmeta" => {
                    if args.len() < 2 {
                        return Err("用法: mtkclient zyb vbmeta <0-3>".into());
                    }
                }
                "erase_data" | "get_build_prop" => {}
                _ => {
                    return Err(format!(
                        "未知 zyb 子命令: {}（可用: seccfg/oem/vbmeta/erase_data/get_build_prop）",
                        args[0]
                    )
                    .into());
                }
            }
        }
        "multi" => {
            if args.is_empty() {
                return Err("用法: mtkclient multi \"<cmd1>;<cmd2>\"".into());
            }
        }
        "run" => {
            if args.is_empty() {
                return Err("用法: mtkclient run <script.json>".into());
            }
        }
        _ => {
            error!("{}", format!("未知命令: {}", cmd).red());
            print_help();
            return Err(format!("未知命令: {}", cmd).into());
        }
    }
    Ok(())
}

/// 判断错误是否为 DA/USB 传输层失败（设备断连、超时、端点错误等）。
///
/// 仅传输层失败才应重置 DA 会话；命令格式错误、参数错误、文件不存在、
/// 解析错误、安全错误等业务错误都属于"DA 会话本身健康"，不应重置
/// （重置会强制下次重新握手/Bypass，纯属浪费）。
///
/// 判定优先级（按 `AppError` 变体精确分支，避免关键词误判）：
/// 1. `AppError::Usb` —— 真正的 USB/DA 传输失败 → 是
/// 2. `AppError::Parse` / `AppError::Security` —— 业务层面错误，绝不可能是传输层
///    失败 → 否（即便消息含"超时"等词也不重置会话，杜绝误判）
/// 3. `AppError::Protocol` / `AppError::Io` → 默认否，仅消息文本明显指示传输失败
///    （含超时/断连/端点错误等关键词，大小写不敏感）时才判为是（兜底）
/// 4. 非 `AppError`（第三方/标准库错误）→ 仅关键词命中才判为是。
///
/// 注意：错误经 `AppError` 包裹后 `Display` 会加"协议错误:""USB 错误:"等前缀，
/// 因此用 `contains` 而非 `starts_with` 判断文本关键词。
/// 判定“是否为 DA/USB 传输层失败”的关键词（大小写不敏感）。
/// 仅作为 Protocol/Io 类错误的兜底判断；Usb/Parse/Security 走精确分支。
const TRANSPORT_KEYWORDS: &[&str] = &[
    "usb 错误",
    "超时",
    "timeout",
    "断开",
    "disconnect",
    "read err",
    "write err",
    "libusb",
    "端点",
    "endpoint",
    "设备未找到",
    "device not found",
];

pub fn is_transport_error(e: &(dyn std::error::Error + 'static)) -> bool {
    match e.downcast_ref::<crate::error::AppError>() {
        Some(crate::error::AppError::Usb(_)) => true,
        Some(crate::error::AppError::Parse(_)) | Some(crate::error::AppError::Security(_)) => false,
        Some(crate::error::AppError::Protocol(_)) | Some(crate::error::AppError::Io(_)) => {
            let err_str = e.to_string().to_lowercase();
            TRANSPORT_KEYWORDS.iter().any(|kw| err_str.contains(kw))
        }
        None => {
            // 非 AppError（第三方/标准库错误）：仅关键词命中才判为传输失败
            let err_str = e.to_string().to_lowercase();
            TRANSPORT_KEYWORDS.iter().any(|kw| err_str.contains(kw))
        }
    }
}

/// 单命令执行入口
pub fn handle_command(
    da: &mut DAXFlash,
    app_config: &AppConfig,
    log_level: u8,
    _quiet_dump: bool,
    preloader_file: &str,
    _context: &USB上下文,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_brom = !da.preloader.is_preloader_mode;
    let mut auto_dumped_file: Option<String> = None;

    let cmd = app_config.command.as_deref().unwrap_or("");

    // 预检查：在 DA 加载之前验证命令有效性，避免浪费时间后才发现命令错误
    validate_command(cmd, &app_config.cmd_args)?;

    // --via preloader 拦截：在 DA 加载之前执行 Pattern 协议
    // DA 加载后 BROM echo 协议失效（设备已进入 DA 模式），必须在握手状态下执行
    if cmd == "reboot" {
        let has_via_preloader = app_config
            .cmd_args
            .windows(2)
            .any(|w| w[0] == "--via" && w[1] == "preloader");
        if has_via_preloader {
            return io::cmd_reboot(da, &app_config.cmd_args, is_brom, !is_brom);
        }
    }

    let has_active_read_resume = cmd == "r" && active_read_resume_exists(&app_config.cmd_args);
    let has_active_write_resume = cmd == "w" && active_write_resume_exists(&app_config.cmd_args);

    // 写入续传：DA 仍在 DRAM 中运行，但 USB 状态可能不一致（上次取消导致 endpoint 残留）
    // 保持 DA 会话复用（不重置），但在后续 handle_command 中通过 drain + xflash_sync 重新同步
    // 不能 reset_session() + 重新加载 DA，因为设备已在 DA 模式，BROM 握手会失败
    if has_active_write_resume {
        debug!("[DA_SESSION] 检测到写入续传，保持 DA 会话，将重新同步 USB 通信");
    }

    // DA 会话复用时跳过 preloader dump / bypass / EMI 加载（DA 仍在运行）
    if !da.daext && is_brom {
        match cmd {
            "dumppreloader" => {
                dump::cmd_dumppreloader(da, _context)?;
                return Ok(());
            }
            _ => {}
        }

        // 1. 获取 target config 判断是否需要 bypass
        let needs_bypass = match da.preloader.get_target_config() {
            Ok(cfg) => {
                info!("{}", cfg.format_info());
                if cfg.needs_bypass() {
                    info!("设备有安全保护，执行 Kamakiri2 bypass...");
                    true
                } else {
                    info!(
                        "设备无安全保护（SBC/SLA/DAA/MemRead 全关），跳过 Kamakiri2，直接进入 DA 模式"
                    );
                    false
                }
            }
            Err(e) => {
                warn!("获取 target config 失败: {}", e);
                warn!("假设 bypass 已处理，直接继续...");
                false
            }
        };

        if needs_bypass {
            da.preloader
                .bypass_security(_context)
                .map_err(|e| format!("bypass_security 失败: {}", e))?;
        }

        // 2. 如果没有指定 preloader 文件，自动从 RAM 提取 preloader（非破坏性 read32）
        //    对齐可用的旧版行为：直接 read32 扫描候选地址的 preloader 签名。
        //    Kamakiri2 bypass 已解除 Mem Read Auth，post-bypass 的 read32 可正常读取；
        //    若设备内存确不可读（极罕见），给出明确指引让用户用 dumppreloader 或 --preloader。
        if preloader_file.is_empty() {
            info!("未指定 --preloader，自动从 RAM 提取 preloader...");
            match da.preloader.dump_preloader_via_brom_read() {
                Ok((data, filename)) => {
                    auto_dumped_file = Some(filename.clone());
                    info!("Preloader 已自动提取: {} ({} 字节)", filename, data.len());
                }
                Err(e) => {
                    return Err(format!(
                        "自动提取 preloader 失败: {}。\n\
                         请手动运行 'dumppreloader' 命令获取文件，\n\
                         或使用 --preloader <文件> 参数指定 preloader 文件。\n\
                         提示: preloader 文件通常位于固件包的 'images' 目录中，\n\
                         文件名类似 preloader_*.bin。",
                        e
                    )
                    .into());
                }
            }
        }
    }

    // Preloader 模式下 DRAM 已由 preloader 初始化，EMI 数据可选
    let effective_preloader = if !preloader_file.is_empty() {
        Some(preloader_file.to_string())
    } else {
        auto_dumped_file.clone()
    };

    if let Some(ref f) = effective_preloader {
        info!("加载 EMI 数据: {}", f);
        if let Err(e) = da.load_preloader_emi(f) {
            return Err(format!("EMI 加载失败: {}", e).into());
        }
        da.preloader_path = Some(f.clone());
    } else if da.preloader.is_preloader_mode {
        debug!("Preloader 模式：跳过 EMI 加载（DRAM 已由 preloader 初始化）");
    } else {
        return Err("未找到 preloader 文件，且自动提取失败".into());
    }

    // DA 会话复用：直接使用，不做心跳验证
    // 刷机匣日志证实 USB 重新打开后 DA 状态机完整，直接 devctrl 查询即可。
    // 心跳验证在 USB 刚重开时可能超时导致误判，触发破坏性的 reconnect/fallback 流程。
    if da.daext {
        if has_active_read_resume {
            debug!("[DA_SESSION] 检测到活跃读取续传，直接续接数据流");
        } else if has_active_write_resume {
            // 写入续传：USB 连接刚重新打开，需要排空残留 + 重新同步 DA 状态机
            debug!("[DA_SESSION] 检测到写入续传状态，重新同步 DA 通信...");
            da.preloader.device.drain_pipes();
            // 发送 xflash_sync 重新同步（写入取消可能导致 DA 等待未完成的写包响应）
            match da.xflash_sync() {
                Ok(_) => debug!("[DA_SESSION] DA 同步成功，可以续写"),
                Err(e) => {
                    warn!("[DA_SESSION] DA 同步失败: {}，尝试 drain 后继续", e);
                    da.preloader.device.drain_pipes();
                }
            }
        } else {
            debug!("DA 会话复用，直接执行命令");
        }
    } else {
        da.upload_da().map_err(|e| format!("DA 加载失败: {}", e))?;
    }

    if log_level >= 2 {
        if let Some(data) = da.get_emi_data() {
            let _ = std::fs::write(crate::system::paths::获取tmp路径("emi_debug.bin"), data);
        }
        if let Some(data) = da.get_extensions_data() {
            let _ = std::fs::write(
                crate::system::paths::获取tmp路径("extensions_debug.bin"),
                &data,
            );
        }
    }

    let args = &app_config.cmd_args;
    let verify = app_config.verify;

    // multi 和 adb 都在 DA 会话已建立后执行，不需要走 execute_single_command
    if cmd == "adb" {
        da.enable_adb_and_reboot()?;
        info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        return Ok(());
    }

    if cmd == "multi" {
        multi::cmd_multi(da, args, app_config, log_level)?;
        return Ok(());
    }

    if cmd == "run" {
        script::cmd_run(da, args, app_config, log_level)?;
        return Ok(());
    }

    execute_single_command(da, cmd, args, verify, log_level, app_config).map_err(|e| {
        // 会话重置策略（关键修正）：只有 DA/USB 传输层真正失效时才重置会话。
        //
        // 旧逻辑靠字符串前缀（"未知命令"/"用法:"）判断"非通信错误"，但错误经 AppError
        // 包裹后 Display 会加"协议错误:""USB 错误:"等前缀，starts_with 判断完全失效，
        // 导致几乎所有命令级错误（参数错、分区找不到、文件错、解析错、安全错）都被误判为
        // DA 通信错误而重置会话——这是没必要的，会强制下次重新握手/Bypass，浪费时间。
        //
        // 新逻辑：默认保留会话；仅当错误确为传输层失败（USB 断连、DA 超时、端点错误等）才重置。
        // 判定：① 用户取消(Ctrl+C) → 保留；② AppError::Usb（真实传输失败）→ 重置；
        // ③ 其余 AppError(Protocol/Security/Parse/Io)与业务错误 → 保留，仅当消息含传输关键词才重置。
        let err_str = e.to_string();
        let is_user_cancel = crate::cancel::requested() || crate::cancel::force_requested();
        let is_transport_error = is_transport_error(&*e);
        if is_transport_error && !is_user_cancel {
            warn!("[DA_SESSION] DA 通信错误，重置会话状态: {}", err_str);
            crate::connection::reset_session();
            // 清理可能残留的 read resume 文件，避免下次启动死循环
            // 注意：write resume 文件不在此时清理，保留供断点续写使用
            if cmd == "r" {
                if let Some(output) = app_config.cmd_args.get(1) {
                    let resume_path = crate::resume::read_resume_path(output);
                    let _ = std::fs::remove_file(&resume_path);
                }
            }
        } else if !is_user_cancel {
            // 非传输错误：DA 会话本身健康，仅记录日志，不重置
            debug!(
                "[DA_SESSION] 非传输错误，保留 DA 会话（不重置）: {}",
                err_str
            );
        }
        e
    })?;

    Ok(())
}

fn active_read_resume_exists(args: &[String]) -> bool {
    let Some(output) = args.get(1) else {
        return false;
    };
    let path = crate::resume::read_resume_path(output);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    if !crate::resume::is_active_read(&content) {
        return false;
    }
    let Some(written) = crate::resume::parse_u64_field(&content, "written=") else {
        return false;
    };
    match std::fs::metadata(output) {
        Ok(metadata) => {
            let file_size = metadata.len();
            // BufWriter 64MB 缓冲：Ctrl+C 时文件可能比 resume 记录小
            // 允许 file_size <= written（文件已刷盘的数据 <= writer 线程已接收的数据）
            // 最大容差 128MB（2×BufWriter 缓冲 + 安全余量）
            file_size <= written && written.saturating_sub(file_size) < 128 * 1024 * 1024
        }
        Err(_) => false,
    }
}

/// 检查是否存在写入断点续传状态文件
fn active_write_resume_exists(args: &[String]) -> bool {
    if args.len() < 2 {
        return false;
    }
    let input_file = &args[1];
    let resume_path = format!("{}.wresume", input_file);
    let Ok(content) = std::fs::read_to_string(&resume_path) else {
        return false;
    };
    let written = crate::resume::parse_u64_field(&content, "written=");
    written.map(|w| w > 0).unwrap_or(false)
}

/// 未完成读取任务信息（供 reboot 等会销毁 DA 会话的命令做保护提示）
#[derive(Debug, Clone)]
pub struct PendingRead {
    /// 输出文件路径（来自 resume 文件的 output= 字段，回退为去 .resume 后缀）
    pub output: String,
    /// 分区总大小（字节）
    pub size: u64,
    /// 已写入字节数
    pub written: u64,
}

/// 扫描指定目录，返回所有"活跃读取未完成"的分区读取任务。
///
/// 判定条件：存在 `<output>.resume` 状态文件且其内容含 `active_read=true`。
/// 该状态由 `readflash_to_file` 在用户取消（Ctrl+C）续传写出；正常完成会被删除，
/// 读取出错则写为 `active_read=false`。只有 `active_read=true` 代表"可续传但未完成"，
/// 此时若执行 reboot 等会销毁 DA 会话的命令，已读取进度将无法续传。
pub fn pending_read_resume_in_dir(dir: &str) -> Vec<PendingRead> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("resume") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !crate::resume::is_active_read(&content) {
            continue;
        }
        let output = content
            .lines()
            .find_map(|line| line.strip_prefix("output="))
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.with_extension("").to_string_lossy().to_string());
        let size = crate::resume::parse_u64_field(&content, "size=").unwrap_or(0);
        let written = crate::resume::parse_u64_field(&content, "written=").unwrap_or(0);
        result.push(PendingRead {
            output,
            size,
            written,
        });
    }
    result
}

/// 扫描当前工作目录的未完成读取任务
pub fn pending_read_resume_cwd() -> Vec<PendingRead> {
    pending_read_resume_in_dir(".")
}

/// 统一命令分发函数（所有命令 match 逻辑的唯一来源）
///
/// 被 `execute_single_command`、`multi::cmd_multi`、`script::dispatch_command` 共同调用，
/// 确保三处命令列表和逻辑完全一致。
pub fn dispatch_cmd(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), crate::error::AppError> {
    match cmd {
        "printgpt" => {
            partition_table::cmd_printgpt(da, log_level);
        }
        "dumppreloader" => {
            info!("dumppreloader 命令请在 BROM 模式下直接执行，无需进入 DA 模式");
        }
        "r" => {
            if args.first().map(|s| s.as_str()) == Some("gpt") {
                let dir = args.get(1).ok_or("用法: mtkclient r gpt <dir>")?;
                partition_table::cmd_read_gpt(da, dir, log_level)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            } else {
                io::cmd_read(da, args)
                    .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
            }
        }
        "rl" => {
            let dir = args.first().ok_or("用法: mtkclient rl <dir>")?;
            partition_table::cmd_read_all(da, dir, app_config)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "wl" => {
            let dir = args.first().ok_or("用法: mtkclient wl <dir>")?;
            partition_table::cmd_write_all(da, dir, verify)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "w" => {
            io::cmd_write(da, args, verify)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "e" => {
            io::cmd_erase(da, args).map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "zyb" => {
            handle_zyb_command(da, args)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "frp" => {
            crate::security::frp::frp_unlock(da)
                .map_err(|e| crate::error::AppError::Security(e.to_string()))?;
        }
        "reboot" => {
            io::cmd_reboot(
                da,
                args,
                !da.preloader.is_preloader_mode,
                da.preloader.is_preloader_mode,
            )
            .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "slot" => {
            io::cmd_slot(da, args).map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "adb" => {
            da.enable_adb_and_reboot()
                .map_err(|e| crate::error::AppError::Usb(e.to_string()))?;
            info!("{}", "ADB 已启用，设备正在重启进入系统".green());
        }
        "peek" => {
            io::cmd_peek(da, args).map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "poke" => {
            io::cmd_poke(da, args).map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        "fs_shell" => {
            // 读 GPT 获取 super 分区地址
            da.read_gpt()
                .map_err(|e| crate::error::AppError::Protocol(format!("读取 GPT 失败: {}", e)))?;
            let gpt_data = da
                .last_gpt_data
                .as_ref()
                .ok_or_else(|| crate::error::AppError::Protocol("GPT 数据不可用".into()))?;
            let gpt_info = crate::partition::gpt::GptInfo::parse(gpt_data)
                .map_err(|e| crate::error::AppError::Parse(format!("解析 GPT 失败: {}", e)))?;

            let super_entry = gpt_info
                .find_partition("super")
                .or_else(|| gpt_info.find_partition("super_b"))
                .ok_or_else(|| {
                    crate::error::AppError::Protocol("GPT 中未找到 super 分区".into())
                })?;
            let super_addr = super_entry.start_addr;

            info!("super 分区地址: 0x{:08X}", super_addr);

            // 构造 read_fn 闭包
            let da_ref = &mut *da;
            let mut read_fn = |offset: u64, len: u64| -> Result<Vec<u8>, String> {
                da_ref.readflash_data_ex(super_addr + offset, len, 8)
            };

            crate::partition::explorer::run_explorer(&mut read_fn)
                .map_err(|e| crate::error::AppError::Protocol(e.to_string()))?;
        }
        _ => {
            error!("{}", format!("未知命令: {}", cmd).red());
            print_help();
            return Err(crate::error::AppError::Protocol(format!(
                "未知命令: {}",
                cmd
            )));
        }
    }

    Ok(())
}

/// 执行单个 DA 命令（不处理 Phase1/Phase2）
///
/// 薄包装：委托给 `dispatch_cmd` 完成实际命令分发。
pub fn execute_single_command(
    da: &mut DAXFlash,
    cmd: &str,
    args: &[String],
    verify: bool,
    log_level: u8,
    app_config: &AppConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    dispatch_cmd(da, cmd, args, verify, log_level, app_config)?;
    Ok(())
}

/// 处理 zyb 子命令
fn handle_zyb_command(
    da: &mut DAXFlash,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient zyb <subcmd> [args]".into());
    }

    let subcmd = args[0].as_str();
    let sub_args = &args[1..];

    match subcmd {
        "vbmeta" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb vbmeta <mode> (0/1/2/3)".into());
            }
            let mode = sub_args[0].parse::<u32>().map_err(|_| "无效模式")?;
            crate::security::vbmeta::vbmeta_disable(da, mode)
                .map_err(|e| format!("修补失败: {}", e))?;
            info!("{}", "vbmeta 已修补".green());
        }
        "seccfg" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb seccfg unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    da.unlock_bootloader()
                        .map_err(|e| format!("解锁失败: {}", e))?;
                    info!("{}", "Bootloader 已解锁".green());
                }
                "lock" => {
                    da.lock_bootloader()
                        .map_err(|e| format!("锁定失败: {}", e))?;
                    info!("{}", "Bootloader 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb seccfg unlock/lock".into()),
            }
        }
        "oem" => {
            if sub_args.is_empty() {
                return Err("用法: mtkclient zyb oem unlock/lock".into());
            }
            match sub_args[0].as_str() {
                "unlock" => {
                    crate::security::frp::frp_unlock(da)
                        .map_err(|e| format!("FRP OEM 解锁失败: {}", e))?;
                    info!("{}", "FRP OEM 已解锁".green());
                }
                "lock" => {
                    crate::security::frp::frp_lock(da)
                        .map_err(|e| format!("FRP OEM 锁定失败: {}", e))?;
                    info!("{}", "FRP OEM 已锁定".green());
                }
                _ => return Err("用法: mtkclient zyb oem unlock/lock".into()),
            }
        }
        "get_build_prop" => {
            buildprop::cmd_buildprop(da, sub_args)?;
        }
        "erase_data" => {
            io::cmd_erase_data(da)?;
        }
        _ => return Err(format!("未知 zyb 子命令: {}", subcmd).into()),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn transport_error_detection() {
        // 真实传输层失败 → 应判为需要重置会话
        assert!(
            is_transport_error(&AppError::Usb("device disconnected".into())),
            "AppError::Usb 应判为传输错误"
        );
        assert!(
            is_transport_error(&AppError::Protocol("read data: read err: timeout".into())),
            "USB 读取超时(含 read err/timeout)应判为传输错误"
        );
        assert!(
            is_transport_error(&AppError::Protocol("写入超时".into())),
            "写入超时(含 超时)应判为传输错误"
        );

        // 业务/命令/参数/文件/解析/安全错误 → 不应重置会话
        assert!(
            !is_transport_error(&AppError::Protocol(
                "用法: mtkclient slot show|a|b（未知槽位操作: shwo）".into()
            )),
            "命令拼写错误不应重置会话"
        );
        assert!(
            !is_transport_error(&AppError::Parse("解析 GPT 失败: ...".into())),
            "解析错误不应重置会话"
        );
        assert!(
            !is_transport_error(&AppError::Security("安全错误: ...".into())),
            "安全错误不应重置会话"
        );
        assert!(
            !is_transport_error(&AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "os error 2"
            ))),
            "文件不存在(os error 2)不应重置会话"
        );
        assert!(
            !is_transport_error(&AppError::Protocol("GPT 中未找到 super 分区".into())),
            "分区找不到不应重置会话"
        );
    }

    #[test]
    fn active_read_resume_detects_sidecar_file() {
        let output =
            std::env::temp_dir().join(format!("cmd_active_resume_{}.img", std::process::id()));
        let output = output.to_string_lossy().to_string();

        // written=4096, file=4096 → 精确匹配，应该检测到
        std::fs::write(
            format!("{}.resume", output),
            "active_read=true\nwritten=4096\n",
        )
        .unwrap();
        std::fs::write(&output, vec![0u8; 4096]).unwrap();
        assert!(active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        // written=8192, file=4096 → BufWriter 未刷完，应该检测到（放宽校验）
        std::fs::write(
            format!("{}.resume", output),
            "active_read=true\nwritten=8192\n",
        )
        .unwrap();
        assert!(active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        // written=200MB, file=0 → 差距 200MB > 128MB 容差，应该不检测
        std::fs::write(
            format!("{}.resume", output),
            format!("active_read=true\nwritten={}\n", 200 * 1024 * 1024),
        )
        .unwrap();
        std::fs::write(&output, vec![0u8; 0]).unwrap();
        assert!(!active_read_resume_exists(&[
            "boot_b".to_string(),
            output.clone()
        ]));

        let _ = std::fs::remove_file(&output);
        let _ = std::fs::remove_file(format!("{}.resume", output));
    }

    #[test]
    fn pending_read_resume_in_dir_detects_active_only() {
        let dir = std::env::temp_dir().join(format!("pending_read_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // 1) active_read=true → 应被检测
        let out1 = dir.join("super.img");
        std::fs::write(
            dir.join("super.img.resume"),
            "output=super.img\nsize=10737418240\nwritten=11403264\nactive_read=true\n",
        )
        .unwrap();

        // 2) active_read=false（读取出错）→ 不应被检测
        let out2 = dir.join("expdb.img");
        std::fs::write(
            dir.join("expdb.img.resume"),
            "output=expdb.img\nsize=10485760\nwritten=4096\nactive_read=false\n",
        )
        .unwrap();

        // 3) 非 resume 文件 → 忽略
        std::fs::write(dir.join("note.txt"), "hello").unwrap();

        let pending = pending_read_resume_in_dir(dir.to_str().unwrap());
        assert_eq!(pending.len(), 1, "只应检测到 active_read=true 的任务");
        assert_eq!(pending[0].output, "super.img");
        assert_eq!(pending[0].size, 10737418240);
        assert_eq!(pending[0].written, 11403264);

        let _ = std::fs::remove_file(out1);
        let _ = std::fs::remove_file(dir.join("super.img.resume"));
        let _ = std::fs::remove_file(out2);
        let _ = std::fs::remove_file(dir.join("expdb.img.resume"));
        let _ = std::fs::remove_file(dir.join("note.txt"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn is_transport_error_precise_by_variant() {
        use crate::error::AppError;
        use std::io;

        // 1) Usb 变体：精确命中，必须重置会话
        assert!(is_transport_error(&AppError::Usb("设备断连".into())));

        // 2) Parse/Security：即便消息含传输关键词也不应重置（回归：旧版按关键词误判）
        assert!(!is_transport_error(&AppError::Parse(
            "读取超时：分区表校验失败".into()
        )));
        assert!(!is_transport_error(&AppError::Security("解锁超时".into())));

        // 3) Protocol/Io：仍走关键词兜底（含超时/端点等，大小写不敏感）
        assert!(is_transport_error(&AppError::Protocol(
            "DA 端点错误 timeout".into()
        )));
        let io_err = AppError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "device not found",
        ));
        assert!(is_transport_error(&io_err));

        // 4) 普通 Protocol 业务错误（无传输关键词）→ 不重置
        assert!(!is_transport_error(&AppError::Protocol(
            "未知命令: foo".into()
        )));
    }
}
