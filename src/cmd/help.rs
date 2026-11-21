//! Help text printing

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
    println!(
        "  zyb seccfg info [文件]        查看 seccfg 状态（hwtype/锁状态/镜像列表，可离线解析）"
    );
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
    println!(
        "  dumppreloader                 从 RAM 提取 Preloader（DA 模式直接读出 / BROM Exploit 回退）"
    );
    println!("  adb                           在 DA 模式下开启 ADB 并重启");
    println!("  peek <addr> [size]            读取设备内存（默认 4 字节），输出 hex dump");
    println!("  poke <addr> <hex_data>        写入数据到设备内存（hex 格式）");
    println!("  fs_shell                      交互式浏览 super 分区文件系统");
    println!(
        "  plan <op>                     离线查看规划结果（≥2 条路径 + 选择理由，不连接设备）"
    );
    println!("  rpmb info|init|read|write     RPMB 密钥烧写与分区读写（set-key 需 --yes，不可逆）");
    println!("  multi \"<cmd1>;<cmd2>;...\"    一次 DA 会话执行多个命令");
    println!("  run <script.json>             从 JSON 脚本文件批量执行命令");
    println!();
    println!("{}", "离线镜像解析（无需连接设备）:".bold());
    println!(
        "  boot <boot.img> [--extract <dir>]   解析 boot/recovery 镜像：头部信息、抽取 kernel/ramdisk/dtbo/dtb、"
    );
    println!(
        "                                    解压 ramdisk、列出 cpio 条目、自动判定 boot/recovery"
    );
    println!("  proinfo <proinfo.bin>          解析 MTK proinfo 分区（含 magic 块表与启发式扫描）");
    println!();
    println!("{}", "XML (V6) DA（新平台 MT6789+）:".bold());
    println!("  xml init [主机标识]           初始化 XML 会话（主机信息 + 运行参数）");
    println!("  xml hw-info / prop <key>      读取硬件信息 / 系统属性");
    println!("  xml efuse-read                读取 efuse");
    println!("  xml efuse-write <hex>         写入 efuse（hex 字节串）");
    println!("  xml security <dev-fw-info|flash-policy <file>|allinone-sig <file>>");
    println!("                               读取固件安全信息 / 下发 policy / 签名");
    println!("  xml r/w/e <part> [file]       XML 协议整分区读 / 写 / 格式化");
    println!("  xml rb/wb/eb <section> <off> <size> [file]  按地址读写擦");
    println!("  xml reboot [disconnect] / boot-mode <mode>  重启 / 设置启动模式");
    println!("  xml flash-update              进入 scatter 刷写流程");
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
        "  {}  https://github.com/WUNELEZI1/mtkclient-rs/releases",
        "更新:".dimmed()
    );
    println!(
        "  {}",
        "本工具完全免费，请勿被骗！禁止倒卖、逆向破解或去除作者信息。"
            .red()
            .dimmed()
    );
    println!(
        "  {}",
        "基于 GPL-3.0-or-later 协议开源，衍生作品须同样以 GPLv3+ 开源。".dimmed()
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
