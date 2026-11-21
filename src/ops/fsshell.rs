//! `fs_shell`: read-only browsing of file systems inside images (capability-driven).
//!
//! Declares `StorageRead` (performs no writes) and delegates "read partition" to `Session`,
//! so real devices and simulation share the same parsing logic.
//!
//! Usage (non-interactive):
//! - `fs_shell boot ls [path]`   — list boot ramdisk (cpio) entries
//! - `fs_shell boot cat <path>`  — print a file inside the ramdisk
//! - `fs_shell boot info`        — boot header info
//! - `fs_shell proinfo show`     — parse proinfo NV data
//!
//! The interactive shell (`fs_shell boot` with no arguments) is still provided by `cmd::io::cmd_imgshell`.

use log::info;

use super::{Op, OpCtx, OpOutcome};
use crate::cmd::bootimg::{decompress, parse as parse_boot, parse_cpio};
use crate::core::capability::{Capability, CapabilitySet};

/// Browse target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsTarget {
    Boot,
    Proinfo,
}

/// Read-only browse action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsAction {
    Info,
    List(String),
    Cat(String),
    Show,
}

pub struct FsShellOp {
    pub target: FsTarget,
    /// Direct image file (offline); when None, read from the device partition
    pub file: Option<String>,
    pub action: FsAction,
}

impl FsShellOp {
    fn load(&self, ctx: &mut OpCtx<'_>) -> Result<Vec<u8>, String> {
        if let Some(f) = &self.file {
            return std::fs::read(f).map_err(|e| format!("读取 {} 失败: {}", f, e));
        }
        let part = match self.target {
            FsTarget::Boot => "boot",
            FsTarget::Proinfo => "proinfo",
        };
        let span = ctx.session.find_partition(part)?;
        info!(
            "读取 {} 分区: 0x{:X}（{} 字节）",
            part, span.addr, span.size
        );
        // Block reads start at partition offset 0
        ctx.session.read_flash(span.addr, span.size)
    }
}

impl Op for FsShellOp {
    fn name(&self) -> &'static str {
        "fs_shell"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageRead)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let data = self.load(ctx)?;
        match self.target {
            FsTarget::Boot => {
                let bi = parse_boot(&data).map_err(|e| format!("boot 头解析失败: {}", e))?;
                let start = bi.ramdisk_off as usize;
                let end = start + bi.ramdisk_size as usize;
                let ramdisk = data
                    .get(start..end)
                    .ok_or_else(|| "ramdisk 区间超出镜像长度".to_string())?;
                let raw = if bi.ramdisk_size == 0 {
                    Vec::new()
                } else {
                    decompress(ramdisk).unwrap_or_else(|_| ramdisk.to_vec())
                };
                let entries = parse_cpio(&raw).map_err(|e| format!("cpio 解析失败: {}", e))?;

                match &self.action {
                    FsAction::Info => {
                        println!("  boot 版本: {:?}", bi.version);
                        println!("  页大小  : {}", bi.page_size);
                        println!(
                            "  kernel  : off=0x{:X} size={}",
                            bi.kernel_off, bi.kernel_size
                        );
                        println!(
                            "  ramdisk : off=0x{:X} size={}",
                            bi.ramdisk_off, bi.ramdisk_size
                        );
                        println!("  cmdline : {}", bi.cmdline);
                        Ok(OpOutcome {
                            summary: format!(
                                "boot 头解析成功（{} 个 ramdisk 条目）",
                                entries.len()
                            ),
                            artifacts: Vec::new(),
                            facts: vec![("entries".into(), entries.len().to_string())],
                        })
                    }
                    FsAction::List(path) => {
                        let prefix = path.trim_matches('/');
                        let mut n = 0;
                        for e in &entries {
                            let name = e.name.trim_start_matches('/');
                            if prefix.is_empty() || name.starts_with(prefix) {
                                println!("{:>10}  {}", e.data.len(), e.name);
                                n += 1;
                            }
                        }
                        Ok(OpOutcome {
                            summary: format!("列出 {} 个条目（过滤: '{}'）", n, prefix),
                            artifacts: Vec::new(),
                            facts: vec![("listed".into(), n.to_string())],
                        })
                    }
                    FsAction::Cat(path) => {
                        let want = path.trim_start_matches('/');
                        let e = entries
                            .iter()
                            .find(|e| e.name.trim_start_matches('/') == want)
                            .ok_or_else(|| format!("ramdisk 内不存在: {}", path))?;
                        print!("{}", String::from_utf8_lossy(&e.data));
                        Ok(OpOutcome {
                            summary: format!("输出 {}（{} 字节）", e.name, e.data.len()),
                            artifacts: Vec::new(),
                            facts: vec![("bytes".into(), e.data.len().to_string())],
                        })
                    }
                    FsAction::Show => Err("boot 目标不支持 show".to_string()),
                }
            }
            FsTarget::Proinfo => {
                if !matches!(self.action, FsAction::Show | FsAction::Info) {
                    return Err("proinfo 目标仅支持 show".to_string());
                }
                let pi = crate::cmd::proinfo::parse(&data);
                if let Some(m) = &pi.magic {
                    println!("  magic  : {}", m);
                }
                if let Some(v) = pi.version {
                    println!("  version: {}", v);
                }
                for b in pi.blocks.iter().chain(pi.scan.iter()) {
                    println!(
                        "    {:<20} offset=0x{:X} length={}",
                        b.name, b.offset, b.length
                    );
                }
                let n = pi.blocks.len() + pi.scan.len();
                Ok(OpOutcome {
                    summary: format!("proinfo 解析成功（{} 项）", n),
                    artifacts: Vec::new(),
                    facts: vec![("entries".into(), n.to_string())],
                })
            }
        }
    }
}
