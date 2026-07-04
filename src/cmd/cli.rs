use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mtkclient")]
#[command(about = "MTKClient Rust 版本 - MTK 设备刷写工具 (by wunelezi & trae)", long_about = None)]
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
        default_value_t = 1,
        help = "DA 加载速度：1=默认（完整协议）、2=快速（跳过可选查询）、3=极速（裸奔）"
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
          printgpt              打印 GPT 分区表 + EMMC 信息\n\
          dumppreloader         从 RAM 提取 Preloader\n\
          r <part> <file>       读取分区到文件 (支持: r gpt <dir>, r boot1 <file>, r boot2 <file>, r rpmb <file>)\n\
          rl <dir>              读取全部分区到目录 (支持 --skip)\n\
          w <part> <file>       写入文件到分区\n\
          wl <dir>              从目录恢复全部分区 (.bin/.img)\n\
          e <part>              擦除分区\n\
          zyb vbmeta <mode>     修补 vbmeta (0/1/2/3)\n\
          zyb seccfg unlock     解锁 Bootloader\n\
          zyb seccfg lock       锁定 Bootloader\n\
          frp                   FRP OEM 解锁\n\
          reboot [system|fastboot|recovery|fastbootd]  重启设备（默认 system）\n\
          slot show/a/b         显示/切换 A/B 槽位\n\
          adb                   在 DA 模式下开启 ADB\n\
          multi \"<cmds>\"         一次 DA 会话执行多个命令 (分号分隔)"
    )]
    pub command: Option<String>,

    #[arg(help = "命令参数")]
    pub args: Vec<String>,
}
