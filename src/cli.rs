use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mtkclient")]
#[command(about = "MTKClient Rust 版本 - MTK 设备刷写工具", long_about = None)]
pub struct Cli {
    #[arg(long = "da2", help = "指定 DA2 文件路径")]
    pub da2_path: Option<String>,

    #[arg(long = "preloader", help = "指定 preloader 文件路径")]
    pub preloader_path: Option<String>,

    #[arg(long = "da", help = "指定 DA loader 文件路径")]
    pub loader_path: Option<String>,

    #[arg(long = "分区类型", help = "指定分区类型")]
    pub parttype: Option<String>,

    #[arg(long = "偏移", help = "指定偏移地址")]
    pub offset: Option<u64>,

    #[arg(long = "长度", help = "指定长度")]
    pub length: Option<u64>,

    #[arg(long = "扇区", help = "指定扇区号")]
    pub sector: Option<u32>,

    #[arg(long = "扇区数", help = "指定扇区数")]
    pub sectors: Option<u32>,

    #[arg(long = "是否校验", help = "写入后校验")]
    pub verify: bool,

    #[arg(
        long = "日志",
        default_value_t = 1,
        help = "日志级别：1=INFO（默认），2=DEBUG（调试信息），3=TRACE（完整原始 hex dump）"
    )]
    pub log_level: u8,

    #[arg(
        long = "静默输出",
        default_value_t = false,
        help = "静默模式（跳过所有 info 输出，只输出错误和最终结果）"
    )]
    pub quiet: bool,

    #[arg(
        long = "静默dump",
        default_value_t = false,
        help = "静默 dump 模式（不打印进度条和 USB 读取日志）"
    )]
    pub quiet_dump: bool,

    #[arg(
        long = "USB日志",
        default_value_t = false,
        help = "启用 USB 通信追踪日志（输出到 usb_debug.log）"
    )]
    pub usb_log: bool,

    #[arg(
        long = "修补da",
        default_value_t = true,
        help = "是否 patch DA（默认开启）"
    )]
    pub patch_da: bool,

    #[arg(
        long = "检测管理员权限",
        default_value_t = false,
        help = "跳过管理员提权检查（已手动以管理员身份运行时使用）"
    )]
    pub no_elevate: bool,

    #[arg(
        long = "工作模式",
        default_value = "brom",
        help = "工作模式：brom（默认，完整BROM流程）、preloader（Preloader VCOM模式，跳过bypass）、auto（自动检测，预留）"
    )]
    pub 工作模式: String,

    #[arg(
        long = "da_x_speed",
        default_value_t = 1,
        help = "DA 加载速度级别：1=默认（完整协议）、2=快速（跳过可选查询）、3=极速（几乎裸奔）"
    )]
    pub da_x_speed: u8,

    #[arg(
        help = "要执行的命令",
        long_help = "可用命令:\n\
          输出分区表        - 打印 GPT 分区表 + EMMC 信息 + 生成 scatter\n\
          提取preloader     - 从 RAM 提取 Preloader\n\
          读分区 <分区> <文件> - 读取分区到文件\n\
          读分区 分区表 <目录>  - 保存 GPT 原始数据到目录\n\
          读取全分区 <目录>   - 读取全部分区到目录\n\
          写入全分区 <目录>   - 从目录恢复全部分区\n\
          写分区 <分区> <文件> - 写入文件到分区\n\
          擦分区 <分区>     - 擦除分区\n\
          禁用avb <模式>    - 修补 vbmeta 分区 (0/1/2/3)\n\
          oem解锁           - FRP OEM 解锁\n\
          解锁bl            - 解锁 Bootloader\n\
          回锁bl            - 锁定 Bootloader\n\
          重启              - 重启设备\n\
          输出scatter       - 打印 scatter 到屏幕并保存文件\n\
          开启USB调试       - 在 DA 模式下开启 ADB"
    )]
    pub command: Option<String>,

    #[arg(help = "命令参数")]
    pub args: Vec<String>,
}
