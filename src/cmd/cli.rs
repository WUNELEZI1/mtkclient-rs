use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mtkclient-rs")]
#[command(about = "MTK 设备底层刷机工具 - 支持 87 种芯片 | Windows 10/11 x64", long_about = None)]
#[command(version = env!("CARGO_PKG_VERSION"))]
pub struct Cli {
    #[arg(long = "da2", help = "指定 DA2 文件路径")]
    pub da2_path: Option<String>,

    #[arg(long = "preloader", help = "指定 preloader 文件路径")]
    pub preloader_path: Option<String>,

    #[arg(long = "da", help = "指定 DA loader 文件路径")]
    pub loader_path: Option<String>,

    #[arg(long = "parttype", help = "指定分区类型")]
    pub parttype: Option<String>,

    #[arg(long = "offset", help = "指定偏移地址")]
    pub offset: Option<u64>,

    #[arg(long = "length", help = "指定长度")]
    pub length: Option<u64>,

    #[arg(long = "sector", help = "指定扇区号")]
    pub sector: Option<u32>,

    #[arg(long = "sectors", help = "指定扇区数")]
    pub sectors: Option<u32>,

    #[arg(long = "verify", help = "写入后校验")]
    pub verify: bool,

    #[arg(
        long = "log",
        default_value_t = 1,
        help = "日志级别：1=INFO（默认），2=DEBUG（调试信息），3=TRACE（完整原始 hex dump）"
    )]
    pub log_level: u8,

    #[arg(
        long = "quiet",
        default_value_t = false,
        help = "静默模式（跳过所有 info 输出，只输出错误和最终结果）"
    )]
    pub quiet: bool,

    #[arg(
        long = "quiet_dump",
        default_value_t = false,
        help = "静默 dump 模式（不打印进度条和 USB 读取日志）"
    )]
    pub quiet_dump: bool,

    #[arg(
        long = "usb_log",
        default_value_t = false,
        help = "启用 USB 通信追踪日志（输出到 usb_debug.log）"
    )]
    pub usb_log: bool,

    #[arg(
        long = "patch_da",
        default_value_t = true,
        help = "是否 patch DA（默认开启）"
    )]
    pub patch_da: bool,

    #[arg(
        long = "no_elevate",
        default_value_t = false,
        help = "跳过管理员提权检查（已手动以管理员身份运行时使用）"
    )]
    pub no_elevate: bool,

    #[arg(
        long = "mode",
        default_value = "brom",
        help = "工作模式：brom（默认，完整BROM流程）、preloader（Preloader VCOM，跳过bypass）、auto（自动检测，预留）"
    )]
    pub mode: String,

    #[arg(
        long = "da_x_speed",
        default_value_t = 3,
        value_parser = clap::value_parser!(u8).range(1..=3),
        help = "DA 加载速度：1=完整协议、2=快速跳过可选查询、3=极速（默认，跳过所有可选查询）"
    )]
    pub da_x_speed: u8,

    #[arg(
        long = "skip",
        help = "rl 命令跳过的分区名（逗号分隔），例如 --skip userdata,metadata,super"
    )]
    pub skip_partitions: Option<String>,

    #[arg(
        help = "要执行的命令",
        long_help = "可用命令:\n\
  分区操作:\n\
    printgpt                    打印 GPT 分区表 + eMMC 信息 + super 动态分区树\n\
    r <part> <file>             读取分区到文件（自动识别动态分区）\n\
      r boot boot.img           读取 boot 分区\n\
      r boot1 boot1.bin         读取 eMMC boot1（特殊分区）\n\
      r system system.img       自动从 super 读取动态分区\n\
      r super --dp system_b system_b.img  读取 super 内指定动态分区\n\
      r gpt <dir>               读取 GPT 原始数据到目录
      w gpt <file>              写回 GPT 原始数据（r gpt 的逆操作，eMMC）\n\
    rl <dir> [--skip <...>]     读取全部分区到目录（逗号分隔跳过列表）\n\
    w <part> <file>             写入文件到分区（自动识别动态分区）\n\
    wl <dir>                    从目录恢复全部分区（匹配 .bin/.img）\n\
    e <part>                    擦除分区\n\
  信息查询:\n\
    zyb get_build_prop [part]   读取 build.prop（默认 system 分区）\n\
    slot show|a|b               显示/切换 A/B 槽位\n\
  安全与解锁:\n\
    zyb seccfg unlock           解锁 Bootloader（seccfg V3/V4 + 自动禁用 vbmeta）\n\
    zyb seccfg lock             锁定 Bootloader（seccfg V3/V4）\n\
    zyb oem unlock              FRP OEM 解锁（修改 frp 分区标志位）\n\
    zyb oem lock                FRP OEM 回锁\n\
    zyb vbmeta <0-3>            修补 vbmeta（0=关闭验证 1=启用 2/3=保留）\n\
    zyb erase_data              清除用户数据 + metadata（恢复出厂设置）\n\
    frp                         FRP OEM 解锁（zyb oem unlock 的别名）\n\
  重启控制:\n\
    reboot [mode] [--via way]   重启设备（默认 system）\n\
      reboot fastboot           重启到 Bootloader（lk fastboot，仅 BROM 模式）\n\
      reboot recovery           重启到 Recovery\n\
      reboot fastbootd          重启到 fastbootd（userspace fastboot）\n\
      reboot meta               重启到 META 模式\n\
      --via misc|para|da|xml|preloader   重启途径（默认 misc）\n\
  其他:\n\
    dumppreloader               从 RAM 提取 Preloader（BROM Exploit）\n\
    adb                         在 DA 模式下开启 ADB 并重启\n\
    peek <addr> [size]          读取设备内存（默认 4 字节），输出 hex dump\n\
    poke <addr> <hex>           写入数据到设备内存\n\
    fs_shell                    交互式浏览 super 分区文件系统\n\
    multi \"<cmd1>;<cmd2>...\"   一次 DA 会话执行多个命令\n\
    run <script.json>           从 JSON 脚本文件批量执行命令"
    )]
    pub command: Option<String>,

    #[arg(
        long = "data-dir",
        help = "指定数据目录（由 GUI 传入；默认使用程序所在目录下的 data/，含 payload/ 与 bin/ 子目录）"
    )]
    pub data_dir: Option<String>,

    #[arg(allow_hyphen_values = true, help = "命令参数")]
    pub args: Vec<String>,
}
