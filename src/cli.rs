use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mtkclient")]
#[command(about = "MTKClient Rust 版本 - MTK 设备刷写工具", long_about = None)]
pub struct Cli {
    #[arg(long = "da2", help = "指定 DA2 文件路径")]
    pub da2_path: Option<String>,

    #[arg(long = "preloader", help = "指定 preloader 文件路径")]
    pub preloader_path: Option<String>,

    #[arg(long = "loader", help = "指定 DA loader 文件路径")]
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
        long = "debugmode",
        default_value_t = false,
        help = "启用调试模式（详细日志 + dump 文件）"
    )]
    pub debug_mode: bool,

    #[arg(
        long = "quiet",
        default_value_t = false,
        help = "静默模式（跳过所有 info 输出，只输出错误和最终结果）"
    )]
    pub quiet: bool,

    #[arg(
        long = "quiet-dump",
        default_value_t = false,
        help = "静默 dump 模式（不打印进度条和 USB 读取日志）"
    )]
    pub quiet_dump: bool,

    #[arg(
        long = "usb-log",
        default_value_t = false,
        help = "启用 USB 通信追踪日志（输出到 usb_debug.log）"
    )]
    pub usb_log: bool,

    #[arg(long = "check-driver", action = clap::ArgAction::SetTrue, help = "检查 WinUSB 驱动状态")]
    pub check_driver: bool,

    #[arg(
        long = "force",
        default_value_t = false,
        help = "强制安装驱动（跳过驱动检查）"
    )]
    pub force: bool,

    #[arg(
        long = "no-device",
        help = "离线模式：不连接设备，直接处理 seccfg 文件"
    )]
    pub no_device: Option<String>,

    #[arg(
        help = "要执行的命令",
        long_help = "可用命令:\n\
          install-drivers - 自动安装 WinUSB 驱动（需要管理员权限）\n\
          printgpt        - 打印 GPT 分区表\n\
          dump-preloader  - 从 RAM 提取 Preloader\n\
          dumpbrom        - 提取 BROM 到文件\n\
          r <分区> <文件> - 读取分区到文件\n\
          w <分区> <文件> - 写入文件到分区\n\
          e <分区>       - 擦除分区\n\
          vbmeta <模式>  - 修补 vbmeta 分区\n\
          unlock          - 解锁 Bootloader\n\
          lock            - 锁定 Bootloader\n\
          reset           - 重置设备\n\
          enable-adb-on-da - 在 DA 模式下开启 ADB\n\
\n\
          离线模式:\n\
          unlock --no-device <seccfg文件> - 离线解锁 seccfg\n\
          lock --no-device <seccfg文件>   - 离线锁定 seccfg\n\
\n\
          批量模式（一次连接执行多个命令）:\n\
          mtkclient-rs printgpt r boot boot.img e userdata"
    )]
    pub command: Option<String>,

    #[arg(help = "命令参数（批量模式下每个子命令的额外参数）")]
    pub args: Vec<String>,
}
