use super::boot_mode::{set_boot_mode_via_para, set_bootloader_message};
use crate::color::Colorize;
use crate::da::DAXFlash;
use log::{info, warn};
use std::time::Duration;

/// 重启设备
///
/// 用法:
///   reboot                          → 正常重启到系统
///   reboot fastboot                 → 重启到 Bootloader（lk fastboot）
///   reboot recovery                 → 重启到 Recovery
///   reboot fastbootd                → 重启到 fastbootd（userspace fastboot）
///   reboot meta                     → 重启到 META 模式
///   reboot <mode> --via <method>    → 指定重启方式
///
/// --via 参数:
///   para      (默认) 通过 para 分区设置 boot_mode（最稳定，需 DA）
///   misc      通过 misc 分区设置 bootloader_message（需 DA）
///   da        通过 DA SHUTDOWN 命令直接重启（需 DA）
///   xml       通过 XML DA SET-BOOT-MODE 重启（新平台，需 DA）
///   preloader 通过 Preloader Pattern 协议（fastboot/meta，纯 Preloader，不加载 DA）
///
/// 设计原则（唯一纯 Preloader 路径）:
///   —— 只有 `reboot <mode> --via preloader` 才是纯 Preloader 路径：在 DA 加载之前拦截，
///       直接走 Preloader Pattern 协议（fastboot/meta）/ Preloader 看门狗（system），
///       全程不加载 DA、不写任何分区。
///   —— 其余所有 reboot（默认 system、fastboot/recovery/fastbootd/meta，以及
///       --via para/misc/da/xml）：
///       一律经过 bypass→upload_da，最终由 DA SHUTDOWN / DA 命令完成重启，绝不走纯 Preloader。
///   —— 例外（降级）：若设备已处于 DA 模式（daext=true，含 DA 复用 / --mode preloader 的
///       DA fallback），则 `--via preloader` 的 Pattern 协议因依赖原始握手态而不可用，
///       自动降级为 DA 默认重启（写 para/misc + DA SHUTDOWN）并提示，不再走必败的 Pattern 路径。
///
/// system 重启说明（reboot / reboot system，默认路径，属于“其余路径”→ 走 DA）:
///   实测（本设备 MT6768）两种原语行为：
///     - 裸看门狗（BROM WRITE32 wdt）在「无 DA 活跃」时可靠重启进系统；
///     - DA SHUTDOWN(bootmode=HOME_SCREEN) 在「DA 活跃」时使用，且**成功发送后必须
///       立即关闭设备端口**（对齐 mtkclient shutdown 末尾的 port.close(reset=True)），
///       否则 DA 软跳回 preloader 后设备因主机仍挂着 USB/串口而停在下载态，表现为
///       “日志显示重启成功但设备没进系统 / 没效果”。
///   分流：daext=false（DA 未加载，防御性兜底）→ 裸看门狗硬复位；daext=true（DA 活跃，含复用会话
///   或刚 upload_da）→ DA SHUTDOWN + close_device。命令仅在 DA 活跃时加载/复用 DA，
///   不写任何分区。
///
/// --via preloader 支持的模式（纯 Preloader，不看 DA）:
///   fastboot  → Pattern FASTBOOT（BROM: 先 reset 到 Preloader，再 Pattern）
///   meta      → Pattern METAMETA（同上）
///   system    → Preloader watchdog 硬复位（同样不加载 DA）
///   recovery/fastbootd → 不支持 Pattern，回退到 para（此时会加载 DA）
///
/// 模式与工作模式的关系:
///   --mode brom / --mode preloader（reboot system，默认，非 --via preloader）:
///     加载 DA → DA SHUTDOWN(bootmode=HOME_SCREEN) → 重启进系统
///   --mode brom:
///     --via preloader: 纯 Preloader（BROM write32 触发看门狗 → 等 Preloader → 握手 → Pattern）
///     --via para/m,isc/da/xml: 加载 DA → 写分区 → 重启
///   --mode preloader:
///     --via preloader: 纯 Preloader（trigger_meta_reboot → Pattern）
///     --via para/misc: 加载 DA → 写分区 → 重启
pub fn cmd_reboot(
    da: &mut DAXFlash,
    args: &[String],
    is_brom: bool,
    is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 解析参数：reboot [mode] [--via misc/para/da/xml/preloader]
    let mut mode = "system";
    let mut via = "para"; // 默认 para（最稳定）

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--via" => {
                via = args.get(i + 1).map(|s| s.as_str()).unwrap_or("para");
                i += 2;
            }
            "system" | "fastboot" | "recovery" | "fastbootd" | "meta" => {
                mode = args[i].as_str();
                i += 1;
            }
            _ => {
                return Err(format!(
                    "未知重启参数: {}。用法: reboot [system|fastboot|recovery|fastbootd|meta] [--via misc|para|da|xml|preloader]",
                    args[i]
                )
                .into());
            }
        }
    }

    // 未完成读取保护：若存在活跃读取续传（Ctrl+C 中断但未完成的分区读取），
    // reboot 会销毁 DA 会话，导致已读取进度无法续传，且复用会话下 DA SHUTDOWN
    // 常返回 0x00010007 而失败。此时保持会话，提示用户先完成分区读取，
    // 而非尝试 DA SHUTDOWN。
    let pending = crate::cmd::pending_read_resume_cwd();
    if !pending.is_empty() {
        warn!(
            "{}",
            "检测到未完成的读取任务，已跳过重启以保持 DA 会话"
                .yellow()
                .bold()
        );
        for p in &pending {
            let pct = if p.size > 0 {
                format!(
                    "（{:.1}% 已完成）",
                    p.written as f64 / p.size as f64 * 100.0
                )
            } else {
                String::new()
            };
            warn!(
                "  - {}：已读取 {}/{} 字节 {}",
                p.output, p.written, p.size, pct
            );
        }
        warn!(
            "{}",
            "请先完成分区读取（重新运行对应的 r <分区> <文件> 命令，会自动从断点续传）后再重启。"
                .yellow()
        );
        warn!(
            "{}",
            "若确定要放弃读取并强制重启，请先删除对应的 .img 与 .resume 文件后重试。".dimmed()
        );
        return Ok(());
    }

    // --via preloader: Pattern 协议路径（支持 fastboot 和 meta）
    // 注意：若设备已处于 DA 模式（daext=true，DA 复用/fallback 后），Pattern 协议依赖的
    // 原始 Preloader/BROM 握手态已不存在，无法执行。此时降级为 DA 默认重启方式
    // （写 para/misc + DA SHUTDOWN），并明确提示，避免静默走必败的 Pattern 路径。
    if via == "preloader" {
        if da.daext {
            warn!(
                "{}",
                format!(
                    "设备处于 DA 模式，--via preloader 的 Pattern 协议不可用；--mode 仍为 {mode}，已降级为经 DA 重启（写 para/misc + DA SHUTDOWN）"
                )
                .yellow()
            );
            via = "para";
        } else {
            return cmd_reboot_via_preloader(da, mode, is_brom, is_preloader);
        }
    }

    // 非 preloader 路径：需要加载 DA
    // Preloader 模式下 via 限制：仅支持 misc、para
    if is_preloader && !matches!(via, "misc" | "para") {
        warn!(
            "Preloader 模式下 via={} 不支持（仅支持 misc/para/preloader），已回退到 para",
            via
        );
        via = "para";
    }

    match mode {
        "system" => {
            // system 重启：优先 DA SHUTDOWN(bootmode=HOME_SCREEN) —— 这是 MTK 在 USB
            // 连接下能可靠重启进系统的唯一方式。裸看门狗硬件复位会让设备重新掉回下载模式
            // （表现为"日志显示重启成功但设备没进系统"），故仅作为 DA 不可用时的兜底。
            //
            // daext=true（DA 已加载，含本次刚 upload_da 或复用既有会话）：直接 DA SHUTDOWN。
            // daext=false（极端：DA 未能加载）：退化为裸看门狗硬复位并明确提示风险。
            if da.daext {
                info!("通过 DA SHUTDOWN 触发重启到系统（bootmode=HOME_SCREEN）...");
                match da.reset_device() {
                    Ok(_) => {
                        info!("{}", "设备正在重启进入系统...".green());
                        // 关键：DA SHUTDOWN 是软跳转，DA 跳回 preloader 后设备若发现主机仍
                        // 挂着 USB/串口（且启动原因为下载模式）会停在下载态，表现为“没效果”。
                        // 必须显式关闭设备端口（对齐 mtkclient shutdown 末尾的
                        // port.close(reset=True)），释放句柄让设备干净重枚举并启动到系统。
                        let _ = da.preloader.device.close_device();
                        // 等待设备物理复位重枚举：MTK DA 收到 SHUTDOWN 后会跳回 preloader
                        // 再启动到系统，主机必须让出 USB 总线足够长时间（对齐 mtkclient
                        // shutdown 末尾的 close + time.sleep）。500ms 过短会导致操作系统侧
                        // 句柄残留、设备停在下载态（日志显示成功但设备不重启）。
                        std::thread::sleep(Duration::from_millis(2000));
                        crate::connection::reset_session();
                        info!("{}", "请保持或断开 USB，等待设备启动到系统".cyan());
                    }
                    Err(e) => {
                        // DA 模式下裸看门狗不可用——BROM echo 协议已随 DA 接管而失效，
                        // 必报 meta_reset: echo 0xD4 不匹配，走裸看门狗只会输出误导性日志。
                        // 直接报告失败并重置会话，请用户重新运行本命令：届时 manager 会重新
                        // Preloader 握手 + 重载 DA，绝大多数情况下可恢复正常重启。
                        warn!("DA SHUTDOWN 失败: {}", e);
                        crate::connection::reset_session();
                        info!(
                            "{}",
                            "DA 重启失败，请重新运行本 reboot 命令（将重新握手并加载 DA 后再试）"
                                .yellow()
                        );
                    }
                }
            } else {
                warn!(
                    "{}",
                    "DA 未加载，退化为裸看门狗硬复位（USB 连接下设备可能重新进入下载模式）"
                        .yellow()
                        .bold()
                );
                watchdog_reboot(da)?;
            }
        }
        "fastboot" => {
            if is_brom {
                match via {
                    "xml" => {
                        info!("通过 XML DA 协议重启到 fastboot...");
                        da.da_xml_reboot_fastboot()
                            .map_err(|e| format!("XML DA fastboot 失败: {}", e))?;
                        info!("{}", "设备已通过 XML DA 重启到 fastboot".green());
                        // 关闭设备端口（对齐 mtkclient shutdown 末尾 close）
                        let _ = da.preloader.device.close_device();
                        std::thread::sleep(Duration::from_millis(2000));
                        crate::connection::reset_session();
                        return Ok(());
                    }
                    "para" => set_boot_mode_via_para(da, mode)?,
                    _ => set_bootloader_message(da, mode)?,
                }
                do_reboot(da, mode)?;
            } else {
                // Preloader 模式非 preloader via：需要加载 DA
                match via {
                    "para" => set_boot_mode_via_para(da, mode)?,
                    _ => set_bootloader_message(da, mode)?,
                }
                do_reboot(da, mode)?;
            }
        }
        "recovery" | "fastbootd" => {
            info!("准备重启到 {} 模式 (via {})...", mode, via);
            match via {
                "da" => {
                    set_bootloader_message(da, mode)?;
                }
                "para" => set_boot_mode_via_para(da, mode)?,
                "xml" => {
                    warn!("XML DA 不支持 {} 模式，回退到 misc", mode);
                    set_bootloader_message(da, mode)?;
                }
                _ => set_bootloader_message(da, mode)?,
            }
            do_reboot(da, mode)?;
        }
        "meta" => {
            info!("准备重启到 meta 模式 (via {})...", via);
            match via {
                "da" => {
                    info!("通过 DA SET_META_BOOT_MODE 重启到 meta...");
                    if let Err(e) = da.set_meta_boot_mode(1) {
                        warn!("SET_META_BOOT_MODE 失败: {}，回退到 misc", e);
                        set_bootloader_message(da, mode)?;
                    }
                }
                "xml" => {
                    da.da_xml_reboot_meta()
                        .map_err(|e| format!("XML DA meta 失败: {}", e))?;
                    info!("{}", "设备已通过 XML DA 重启到 meta".green());
                    // 关闭设备端口（对齐 mtkclient shutdown 末尾 close）
                    let _ = da.preloader.device.close_device();
                    std::thread::sleep(Duration::from_millis(500));
                    crate::connection::reset_session();
                    return Ok(());
                }
                "para" => set_boot_mode_via_para(da, mode)?,
                _ => set_bootloader_message(da, mode)?,
            }
            do_reboot(da, mode)?;
        }
        _ => return Err(format!("不支持的重启模式: {}", mode).into()),
    }

    Ok(())
}

/// --via preloader: 通过 Pattern 协议重启设备
///
/// 支持的 mode: fastboot, meta
/// 不支持的 mode (recovery/fastbootd/system): 回退到 para
///
/// BROM 模式流程（DA 加载前拦截，设备仍在 BROM 握手状态）:
/// 1. trigger_meta_reboot: write32(wdt+0x14, 0x00001209) 触发看门狗重启
/// 2. 关闭串口 → 等待设备重枚举为 Preloader VCOM
/// 3. open_raw → 读 READY → 发送模式标识 → DISCONNECT
///
/// Preloader 模式流程（同上）
fn cmd_reboot_via_preloader(
    da: &mut DAXFlash,
    mode: &str,
    is_brom: bool,
    _is_preloader: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::cmd::preloader_boot_mode::{self, BootMode};

    // Pattern 协议支持的模式映射
    let boot_mode = match mode {
        "fastboot" => BootMode::Fastboot,
        "meta" => BootMode::Meta,
        _ => {
            // recovery/fastbootd/system 不支持 Pattern，回退到 para
            warn!(
                "{} 模式不支持 --via preloader（Pattern 协议无对应标识），回退到 --via para",
                mode
            );
            match mode {
                "system" => {
                    // system 模式：统一走硬件看门狗硬复位（与默认 reboot system 一致）
                    info!("通过看门狗硬复位重启到系统...");
                    if da.preloader.chip.is_none() {
                        if let Err(e) = da.preloader.get_hw_code() {
                            warn!("获取芯片信息失败（{}），无法触发看门狗", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                            return Ok(());
                        }
                    }
                    match da.preloader.trigger_meta_reboot() {
                        Ok(_) => {
                            info!("{}", "看门狗已触发，设备正在硬件复位...".green());
                            crate::connection::reset_session();
                        }
                        Err(e) => {
                            warn!("看门狗触发失败: {}", e);
                            crate::connection::reset_session();
                            info!("{}", "请手动重启设备".yellow());
                        }
                    }
                }
                "recovery" | "fastbootd" | "meta" => {
                    set_boot_mode_via_para(da, mode)?;
                    do_reboot(da, mode)?;
                }
                _ => {}
            }
            return Ok(());
        }
    };

    let mode_name = boot_mode.name();
    let tag = if is_brom { "BROM" } else { "Preloader" };
    info!("通过 Preloader Pattern 协议重启到 {}...", mode_name);

    // 步骤 1: trigger_meta_reboot 触发看门狗重启
    info!("  [{}] 步骤 1/2: 触发看门狗重启...", tag);
    match da.preloader.trigger_meta_reboot() {
        Ok(_) => info!("  看门狗已触发，设备正在重启..."),
        Err(e) => warn!("  看门狗触发失败: {}，继续等待设备重枚举", e),
    }
    let _ = da.preloader.device.close_device();
    crate::connection::reset_session();
    std::thread::sleep(Duration::from_millis(1000));

    // 步骤 2: 等待 Preloader VCOM → Pattern 协议
    info!(
        "  [{}] 步骤 2/2: 等待 Preloader VCOM → Pattern 协议...",
        tag
    );
    match preloader_boot_mode::try_preloader_pattern(boot_mode) {
        Ok(_) => {
            info!(
                "{}",
                format!("  设备已通过 Pattern 重启到 {}", mode_name)
                    .green()
                    .bold()
            );
            return Ok(());
        }
        Err(e) => {
            warn!("  Pattern 发送失败: {}，设备可能已进入目标模式", e);
            return Ok(());
        }
    }
}

/// 裸看门狗硬复位兜底（MTK BROM WRITE32 wdt+0x14 = 0x1209）。
///
/// 注意：USB 连接下裸看门狗会让设备重新掉回 Preloader/BROM 下载模式而非进系统，
/// 因此仅作为 DA SHUTDOWN 不可用时的最后兜底，并明确提示用户该风险。
fn watchdog_reboot(da: &mut DAXFlash) -> Result<(), Box<dyn std::error::Error>> {
    if da.preloader.chip.is_none() {
        if let Err(e) = da.preloader.get_hw_code() {
            warn!("获取芯片信息失败（{}），无法触发看门狗", e);
            crate::connection::reset_session();
            info!(
                "{}",
                "请手动重启设备（断开 USB 重新连接或按电源键）".yellow()
            );
            return Ok(());
        }
    }
    match da.preloader.trigger_meta_reboot() {
        Ok(_) => {
            info!("{}", "看门狗已触发，设备正在硬件复位...".green());
            crate::connection::reset_session();
            info!(
                "{}",
                "若设备仍停在下载模式（VCOM/Preloader），请手动重启或断开 USB 后重连".cyan()
            );
        }
        Err(e) => {
            warn!("看门狗触发失败: {}", e);
            crate::connection::reset_session();
            info!(
                "{}",
                "请手动重启设备（断开 USB 重新连接或按电源键）".yellow()
            );
        }
    }
    Ok(())
}

/// 执行实际重启
/// 使用刷机匣协议（SYNC + CMD_SHUTDOWN），失败则提示用户手动重启
fn do_reboot(da: &mut DAXFlash, mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    match da.reset_device() {
        Ok(()) => {
            info!("{}", format!("设备已重启到 {} 模式", mode).green());
            // 同 reboot system：DA SHUTDOWN 为软跳转，必须关闭设备端口释放句柄，
            // 否则设备停在下载态（主机仍挂 USB），表现为“没效果”。对齐 mtkclient close。
            let _ = da.preloader.device.close_device();
            std::thread::sleep(Duration::from_millis(500));
            info!("{}", "请断开 USB 连接，等待设备自动重启".cyan());
            crate::connection::reset_session();
            Ok(())
        }
        Err(e) => {
            warn!("重启失败: {}", e);
            crate::connection::reset_session();
            info!("{}", format!(
                "misc/para 分区已写入，请手动重启设备（断开 USB 重新连接或按电源键重启），设备将启动到 {} 模式",
                mode
            ).yellow());
            Ok(())
        }
    }
}
