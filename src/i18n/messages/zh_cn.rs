//! Simplified Chinese (zh-CN) message table and help text.

pub(super) const MESSAGES: &[&str] = &[
    // os_unsupported
    "不支持的操作系统: Windows {0}.{1} (Build {2})。本程序需要 Windows 10 或 Windows 11。",
    // arch_unsupported
    "错误: 本程序仅支持 64-bit (x86_64) 架构。",
    // os_warning
    "警告: 本程序仅在 Windows 10/11 上经过充分测试。",
    // get_updates
    "获取更新:",
    // free_tool_notice
    "本工具完全免费，请勿被骗！禁止任何形式的倒卖、破解或去除作者信息。违者必究。",
    // data_dir_missing
    "--data-dir 路径不存在或不是目录: {0}",
    // elevate_request
    "[MAIN] 需要管理员权限以安装 WinUSB 驱动，正在请求提权...",
    // elevate_failed
    "[MAIN] 提权失败: {0}",
    // elevate_hint
    "[MAIN] 请右键以管理员身份运行本程序（否则将无法切换 WinUSB）",
    // connected_preloader
    "连接成功 (Preloader 模式)",
    // connected_brom
    "连接成功 (BROM 模式)",
    // no_preloader_forced_dump
    "未指定 --preloader，将强制从设备 dump 并覆盖同名文件",
    // adb_rebooting
    "ADB 已启用，设备正在重启进入系统",
    // unknown_command
    "未知命令: {0}",
    // usage_r_gpt
    "用法: mtkclient r gpt <dir>",
    // usage_rl
    "用法: mtkclient rl <dir>",
    // usage_wl
    "用法: mtkclient wl <dir>",
    // usage_w_gpt
    "用法: mtkclient w gpt <文件>",
    // read_gpt_failed
    "读取 GPT 失败: {0}",
    // gpt_unavailable
    "GPT 数据不可用",
    // parse_gpt_failed
    "解析 GPT 失败: {0}",
    // no_super_in_gpt
    "GPT 中未找到 super 分区",
    // log_wait_device
    "等待设备连接 (BROM: Vol+ + Vol- + Power)",
    // log_auto_detect
    "[AUTO] 自动检测模式：等待 BROM 或 Preloader 设备出现...",
    // log_brom_usb_found
    "[AUTO] 检测到 BROM WinUSB 设备",
    // log_brom_com_ok
    "[AUTO] BROM 串口握手成功",
    // log_da_loaded
    "DA 加载完成",
    // log_da_ext_ready
    "DA Extensions 加载成功，存储类型已设置为 {0}",
    // log_skip_existing
    "  文件已存在且完整 ({0} 字节)，跳过读取",
    // log_saved_to
    "  已保存到: {0}",
    // log_write_start
    "写入文件 {0} 到分区 {1}...",
    // log_write_done
    "  写入完成: {0} 字节写入分区 {1}",
    // log_verify_ok
    "  校验通过 (共 {0} 字节)",
    // log_erase_start
    "正在擦除 {0}...",
    // log_erased
    "{0} 已擦除",
    // log_format_start
    "正在格式化 {0} (f2fs)...",
    // log_format_done
    "{0} 已格式化 (f2fs)",
    // log_unlock_start
    "开始在线解锁 Bootloader...",
    // log_unlock_ok
    "Bootloader 解锁成功",
    // log_lock_start
    "开始在线锁定 Bootloader...",
    // log_lock_ok
    "Bootloader 锁定成功",
    // log_rpmb_key_ok
    "RPMB 密钥已写入并通过回显校验",
    // plan_usage
    "用法: mtkclient plan <printgpt|read|write|erase|seccfg|rpmb> [--chip 0xNNNN] [--da 文件]",
    // plan_unknown_op
    "未知操作: {0}（可用: printgpt/read/write/erase/seccfg/rpmb）",
    // plan_bad_mode
    "非法 --mode: {0}（brom/preloader/auto）",
];

pub(super) const HELP: &str = r#"mtkclient-rs
MTK 设备底层刷机工具 - 支持 87 种芯片 | Windows 10/11 x64

用法: mtkclient-rs [选项] <命令> [参数]

命令:
  分区操作:
    printgpt                    打印 GPT 分区表 + eMMC 信息 + super 动态分区树
    r <part> <file>             读取分区到文件（自动识别动态分区）
      r boot boot.img           读取 boot 分区
      r boot1 boot1.bin         读取 eMMC boot1（特殊分区）
      r system system.img       自动从 super 读取动态分区
      r super --dp system_b system_b.img  读取 super 内指定动态分区
      r gpt <dir>               读取 GPT 原始数据到目录
      w gpt <file>              写回 GPT 原始数据（r gpt 的逆操作，eMMC）
    rl <dir> [--skip <...>]     读取全部分区到目录（逗号分隔跳过列表）
    w <part> <file>             写入文件到分区（自动识别动态分区）
    wl <dir>                    从目录恢复全部分区（匹配 .bin/.img）
    e <part>                    擦除分区
  信息查询:
    zyb get_build_prop [part]   读取 build.prop（默认 system 分区）
    slot show|a|b               显示/切换 A/B 槽位
  安全与解锁:
    zyb seccfg unlock           解锁 Bootloader（seccfg V3/V4 + 自动禁用 vbmeta）
    zyb seccfg lock             锁定 Bootloader（seccfg V3/V4）
    zyb seccfg info [文件]      查看 seccfg 状态（hwtype/锁状态/镜像列表）
    zyb oem unlock              FRP OEM 解锁（修改 frp 分区标志位）
    zyb oem lock                FRP OEM 回锁
    zyb vbmeta <0-3>            修补 vbmeta（0=关闭验证 1=启用 2/3=保留）
    unlock [--yes]              一键解锁：seccfg unlock + 关闭 vbmeta 校验
    fs_shell boot ls|cat|info   浏览 boot ramdisk（只读，能力门控）
    fs_shell proinfo show       解析 proinfo NV 块
    zyb erase_data              清除用户数据 + metadata（恢复出厂设置）
    frp                         FRP OEM 解锁（zyb oem unlock 的别名）
  重启控制:
    reboot [mode] [--via way]   重启设备（默认 system）
      reboot fastboot           重启到 Bootloader（lk fastboot，仅 BROM 模式）
      reboot recovery           重启到 Recovery
      reboot fastbootd          重启到 fastbootd（userspace fastboot）
      reboot meta               重启到 META 模式
      --via misc|para|da|xml|preloader   重启途径（默认 misc）
  其他:
    chain <goal>                显示目标的智能利用链规划（read|rp|write|peek|dump），
    plan <op>                   离线查看规划路径（≥2 条）与选择理由，不连接设备
                                按设备状态动态选择 kamakiri/carbonara 等最优路径
    --sim <预设>                离线对着虚拟 MTK 设备跑（无需真机，多用于回归）：
                                v5-clean|v5-sla|v5-daa|v6|ufs-ab|flaky|unplug；
                                也支持 --sim ./my.sim 或 --port mock://<预设>
    avb <info|disable> <file>   离线 avbtool：解析 vbmeta / 禁用校验（自研，无需设备）
    fs_shell boot [file]        交互式浏览 boot 镜像 ramdisk（cpio）：ls/cat/extract/quit；
                                省略文件时自动读当前 A/B 槽位的 boot 分区
    fs_shell proinfo [file]     交互式查看 MTK proinfo 分区（NV 数据）
    dumppreloader               从 RAM 提取 Preloader（BROM Exploit）
    dumpbrom [file]             提取 BROM 本体（0x0 起 128KB，供逆向分析）
    genkey                      生成 32 字节随机 RPMB 密钥（hex 输出）
    rpmb info|init|read|write        RPMB 密钥烧写与分区读写
    adb                         在 DA 模式下开启 ADB 并重启
    peek <addr> [size]          读取设备内存（默认 4 字节），输出 hex dump
    poke <addr> <hex>           写入数据到设备内存
    fs_shell                    交互式浏览 super 分区文件系统
    multi "<cmd1>;<cmd2>..."    一次 DA 会话执行多个命令
    run <script.json>           从 JSON 脚本文件批量执行命令
  help                          显示本帮助

选项:
      --da <file>          指定 DA loader 文件路径
      --da2 <file>         指定 DA2 文件路径
      --preloader <file>   指定 preloader 文件路径
      --parttype <type>    指定分区类型
      --offset <n>         指定偏移地址
      --length <n>         指定长度
      --sector <n>         指定扇区号
      --sectors <n>        指定扇区数
      --verify             写入后校验
      --log <1-3>          日志级别：1=INFO（默认），2=DEBUG，3=TRACE（完整原始 hex dump）
      --quiet              静默模式（只输出错误和最终结果）
      --quiet_dump         静默 dump 模式（不打印进度条和 USB 读取日志）
      --usb_log            启用 USB 通信追踪日志（输出到 usb_debug.log）
      --log_file <FILE>    全部日志同步写入指定文件（追加模式）
      --patch_da           是否 patch DA（默认开启）
      --no_elevate         跳过管理员提权检查
      --mode <mode>        工作模式：brom（默认）、preloader、auto（预留）
      --da_x_speed <1-3>   DA 加载速度：1=完整协议、2=快速、3=极速（默认）
      --skip <parts>       rl 命令跳过的分区名（逗号分隔）
      --data-dir <dir>     指定数据目录（默认程序所在目录下的 data/）
      --language <LANG>    输出语言：en-US（默认）、zh-CN、fr-FR、ru-RU、es-ES、ar-SA
  -h, --help               显示帮助
  -V, --version            显示版本
"#;
