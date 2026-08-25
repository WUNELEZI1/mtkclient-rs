//! 用户命令入口
//!
//! 子模块：
//! - `cli`            — 命令行参数解析
//! - `dump`           — dumppreloader 等镜像提取
//! - `io`             — r/w/e/reboot/slot 等 IO 命令
//! - `partition_table` — printgpt / rl / wl 等分区表相关
//! - `handle`         — 单命令执行入口 handle_command（Phase1/Phase2）
//! - `dispatch`       — 统一命令分发 dispatch_cmd / execute_single_command
//! - `zyb`            — zyb 子命令处理 handle_zyb_command
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

// 重型入口函数下放到子模块，保持在 500 行以内
mod dispatch;
mod handle;
mod zyb;

use crate::color::Colorize;
use log::error;

// 重新导出从子模块移动过来的 pub 项，保持 `crate::cmd::<name>` 路径不变
pub use dispatch::{dispatch_cmd, execute_single_command};
pub use handle::handle_command;
pub(crate) use zyb::handle_zyb_command;

pub fn print_help() {
    use crate::color::Colorize;

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
    println!("  w gpt <文件>              写回 GPT 原始数据（r gpt 的逆操作，eMMC）");
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
    println!("  dumppreloader                 从 RAM 提取 Preloader（DA 模式直接读出 / BROM Exploit 回退）");
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

#[cfg(test)]
mod tests;
