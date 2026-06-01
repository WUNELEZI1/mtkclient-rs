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

    #[arg(
        long = "patch-da",
        default_value_t = true,
        help = "是否 patch DA（默认开启）"
    )]
    pub patch_da: bool,

    #[arg(
        help = "要执行的命令",
        long_help = "可用命令:\n\
          printgpt        - 打印 GPT 分区表\n\
          dump-preloader  - 从 RAM 提取 Preloader\n\
          dumpbrom        - 提取 BROM 到文件\n\
          r <分区> <文件> - 读取分区到文件\n\
          r gpt <目录>    - 保存 GPT 原始数据到目录\n\
          rl <目录>       - 读取全部分区到目录\n\
          w <分区> <文件> - 写入文件到分区\n\
          e <分区>       - 擦除分区\n\
          vbmeta <模式>  - 修补 vbmeta 分区\n\
          frp             - FRP OEM 解锁\n\
          unlock          - 解锁 Bootloader\n\
          lock            - 锁定 Bootloader\n\
          reset           - 重置设备\n\
          print-scatter   - 打印 scatter 到屏幕并保存文件\n\
          enable-adb-on-da - 在 DA 模式下开启 ADB"
    )]
    pub command: Option<String>,

    #[arg(help = "命令参数")]
    pub args: Vec<String>,
}
