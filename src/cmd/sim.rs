//! `--sim` / `--port mock://...`：离线仿真入口。
//!
//! 流程：加载设备画像 → 构建虚拟设备 → 构建会话 → 交给 Executor 走
//! Orchestrator → 打印决策链 → 逐步执行 → op 输出。
//!
//! 支持的命令（与真机同一份 ops 代码）：
//! `printgpt` / `r` / `w` / `e` / `seccfg unlock|lock` / `vbmeta <mode>` / `unlock`
//!
//! 例：
//! ```text
//! mtkclient-rs --sim v5-clean printgpt
//! mtkclient-rs --sim v5-clean r boot_a boot.img --yes
//! mtkclient-rs --sim flaky-write w boot_a boot.img --yes
//! mtkclient-rs --sim v5-clean --log 3 unlock --yes
//! ```

use log::info;

use crate::core::plan::Mode;
use crate::exec::Executor;
use crate::ops::erase::EraseOp;
use crate::ops::printgpt::PrintGptOp;
use crate::ops::read::ReadOp;
use crate::ops::seccfg::{SeccfgAction, SeccfgOp};
use crate::ops::unlock::UnlockOp;
use crate::ops::vbmeta::VbmetaOp;
use crate::ops::write::WriteOp;
use crate::session::SimSession;
use crate::system::config::AppConfig;
use crate::transport::vdev::{ChaosConfig, DeviceProfile, VirtualDevice};

/// 仿真入口。
pub fn run_sim(
    app_config: &AppConfig,
    spec: &str,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let cmd = app_config.command.as_deref().unwrap_or("");
    let args = &app_config.cmd_args;

    let profile = load_profile(spec)?;
    let virt = VirtualDevice::new(profile.clone());
    let virt = if app_config.sim_chaos {
        let cfg = ChaosConfig::typical(app_config.sim_chaos_seed);
        println!(
            "[sim] 混沌注入: seed=0x{:X} bitflip={}‰ drop={}‰ silence={}‰ glitch={}‰",
            cfg.seed,
            cfg.bitflip_permille,
            cfg.drop_permille,
            cfg.silence_permille,
            cfg.glitch_permille
        );
        virt.with_chaos(cfg)
    } else {
        virt
    };
    println!(
        "{}",
        format!(
            "[sim] 虚拟设备: {}  hw=0x{:04X}  da=0x{:04X}  proto={}  safety=0x{:X}",
            profile.name,
            profile.hw_code,
            profile.da_code,
            if profile.v6 { "V6" } else { "V5" },
            profile.safety
        )
    );

    let da = match app_config.da_path.as_deref() {
        Some(path) => std::fs::read(path).map_err(|e| format!("读取 DA '{}' 失败: {}", path, e))?,
        None => {
            info!("[sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）");
            synthetic_da(profile.da_code, profile.v6)
        }
    };

    let mut session = SimSession::new(virt).with_da(da);
    if let Some(state) = app_config.sim_state.as_deref()
        && std::path::Path::new(state).exists()
    {
        session.load_state(state)?;
        println!("[sim] 已载入 flash 状态: {}", state);
    }

    let mode = match app_config.work_mode {
        crate::system::config::WorkMode::Brom => Mode::Brom,
        crate::system::config::WorkMode::Preloader => Mode::Preloader,
        crate::system::config::WorkMode::Auto => Mode::Auto,
    };
    // 仿真默认 auto：没人会在仿真里要求"只准走 BROM"（除非显式给 --mode）
    let explicit_mode = std::env::args().any(|a| a == "--mode");
    let mode = if explicit_mode { mode } else { Mode::Auto };

    let interactive = !app_config.assume_yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    let mut exec = Executor::new(mode)
        .with_assume_yes(app_config.assume_yes)
        .with_interactive(interactive);

    let report = match cmd {
        "printgpt" => exec.execute(&PrintGptOp, &mut session, true, log_level),
        "r" | "read" => {
            let part = args.first().ok_or("用法: --sim <预设> r <part> <file>")?;
            let out = args
                .get(1)
                .ok_or("用法: --sim <预设> r <part> <file>")?
                .clone();
            exec.execute(
                &ReadOp {
                    part: part.clone(),
                    out,
                },
                &mut session,
                true,
                log_level,
            )
        }
        "w" | "write" => {
            let part = args
                .first()
                .ok_or("用法: --sim <预设> w <part> <file> [--verify]")?;
            let file = args
                .get(1)
                .ok_or("用法: --sim <预设> w <part> <file> [--verify]")?
                .clone();
            let verify = app_config.verify || args.iter().any(|a| a == "--verify");
            exec.execute(
                &WriteOp {
                    part: part.clone(),
                    file,
                    verify,
                },
                &mut session,
                true,
                log_level,
            )
        }
        "e" | "erase" => {
            let part = args.first().ok_or("用法: --sim <预设> e <part>")?;
            exec.execute(
                &EraseOp {
                    part: part.clone(),
                    check: true,
                },
                &mut session,
                true,
                log_level,
            )
        }
        "seccfg" => {
            let action = match args.first().map(|s| s.as_str()) {
                Some("unlock") => SeccfgAction::Unlock,
                Some("lock") => SeccfgAction::Lock,
                _ => return Err("用法: --sim <预设> seccfg unlock|lock".into()),
            };
            exec.execute(&SeccfgOp { action }, &mut session, true, log_level)
        }
        "vbmeta" => {
            let mode: u32 = args
                .first()
                .ok_or("用法: --sim <预设> vbmeta <0-3>")?
                .parse()
                .map_err(|_| "vbmeta 模式必须是 0-3")?;
            exec.execute(&VbmetaOp { mode }, &mut session, true, log_level)
        }
        "unlock" => exec.execute(&UnlockOp, &mut session, true, log_level),
        "fs_shell" => {
            use crate::ops::fsshell::{FsAction, FsShellOp, FsTarget};
            let target = match args.first().map(|s| s.as_str()) {
                Some("boot") => FsTarget::Boot,
                Some("proinfo") => FsTarget::Proinfo,
                _ => {
                    return Err(
                        "用法: --sim <预设> fs_shell boot|proinfo <ls|cat|info|show> [arg]".into(),
                    );
                }
            };
            let action = match args.get(1).map(|s| s.as_str()) {
                Some("ls") => FsAction::List(args.get(2).cloned().unwrap_or_default()),
                Some("cat") => {
                    FsAction::Cat(args.get(2).ok_or("用法: fs_shell boot cat <path>")?.clone())
                }
                Some("info") => FsAction::Info,
                Some("show") => FsAction::Show,
                _ => {
                    return Err(
                        "用法: --sim <预设> fs_shell boot|proinfo <ls|cat|info|show> [arg]".into(),
                    );
                }
            };
            exec.execute(
                &FsShellOp {
                    target,
                    file: None,
                    action,
                },
                &mut session,
                true,
                log_level,
            )
        }
        other => {
            return Err(format!(
                "仿真模式不支持命令 '{}'（支持: printgpt/r/w/e/seccfg/vbmeta/unlock）",
                other
            )
            .into());
        }
    }
    .map_err(|e| {
        // 失败也要把设备侧状态落盘，便于"重连后续传"
        if let Some(state) = app_config.sim_state.as_deref() {
            let _ = session.save_state(state);
        }
        // 失败也要把已获得的设备侧线索带出来，便于定位
        format!("{}\n[sim] 设备事件:\n{}", e, dump_events(&session))
    })?;

    if let Some(state) = app_config.sim_state.as_deref() {
        session.save_state(state)?;
    }

    info!(
        "[sim] 执行完成：{} 步，cost={}",
        report.executed.len(),
        report.decision.selected_cost
    );
    println!("[sim] 结果: {}", report.outcome.summary);
    if log_level >= 2 {
        println!("[sim] 执行步骤: {}", report.executed.join(" -> "));
        for (step, gained) in &report.gains {
            println!("[sim]   {} => [{}]", step, gained.join(","));
        }
        for (k, v) in &report.outcome.facts {
            println!("[sim]   {}={}", k, v);
        }
        println!("{}", dump_events(&session));
    }
    Ok(())
}

fn dump_events(session: &SimSession) -> String {
    let mut out = String::from("[sim] 设备侧事件:");
    for e in session.events() {
        out.push_str("\n  · ");
        out.push_str(e);
    }
    out
}

fn load_profile(spec: &str) -> Result<DeviceProfile, Box<dyn std::error::Error>> {
    let name = spec.strip_prefix("mock://").unwrap_or(spec);
    let name = name.trim();
    if name.is_empty() {
        return Err("仿真规格为空：请给出预设名或 .sim 文件路径".into());
    }
    if let Some(p) = DeviceProfile::preset(name) {
        return Ok(p);
    }
    let path = std::path::Path::new(name);
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取画像 '{}' 失败: {}", name, e))?;
        return DeviceProfile::from_text(&text).map_err(|e| format!("画像解析失败: {}", e).into());
    }
    let presets = [
        "v5-clean",
        "v5-sla",
        "v5-daa",
        "v6",
        "ufs-ab",
        "flaky",
        "flaky-write",
        "unplug",
        "no-emmc-info",
        "ab-unknown-slot",
    ];
    Err(format!(
        "未知仿真规格 '{}'（可用预设: {}；或给出 .sim 文件路径）",
        name,
        presets.join(", ")
    )
    .into())
}

/// 合成一个最小 DA：头部含 `MTK_DA_v5` / `v6` 标识 + 0x6C 处 da_code。
///
/// 仿真设备不校验签名，但协议版本判定（`ProtocolVersion::from_da_header`）
/// 与 `da_code` 解析都会真实执行，因此合成 DA 必须带上这些字段。
fn synthetic_da(da_code: u16, v6: bool) -> Vec<u8> {
    let mut da = vec![0u8; 4096];
    let tag = if v6 { b"MTK_DA_v6" } else { b"MTK_DA_v5" };
    da[..9].copy_from_slice(tag);
    // load table 首项：base 0x6C，entry 内 [2..4] 为 da_code
    da[0x6C + 2..0x6C + 4].copy_from_slice(&da_code.to_le_bytes());
    da
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_da_has_protocol_tag() {
        use crate::core::device::ProtocolVersion;
        let v5 = synthetic_da(0x6768, false);
        assert_eq!(ProtocolVersion::from_da_header(&v5), ProtocolVersion::V5);
        let v6 = synthetic_da(0x6983, true);
        assert_eq!(ProtocolVersion::from_da_header(&v6), ProtocolVersion::V6);
    }

    #[test]
    fn preset_lookup_and_unknown_hint() {
        assert!(load_profile("mock://v6").is_ok());
        assert!(load_profile("mock://flaky-write").is_ok());
        let err = load_profile("mock://no-such").unwrap_err().to_string();
        assert!(err.contains("可用预设"));
    }
}
