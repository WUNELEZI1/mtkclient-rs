//! `fs_shell`：只读地浏览镜像内的文件系统（能力驱动版本）。
//!
//! 声明 `StorageRead`（不做任何写操作），把"读分区"这一步交给 `Session`，
//! 因此真机与仿真共用同一份解析逻辑。
//!
//! 用法（非交互）：
//! - `fs_shell boot ls [path]`   —— 列出 boot ramdisk（cpio）条目
//! - `fs_shell boot cat <path>`  —— 输出 ramdisk 内某个文件
//! - `fs_shell boot info`        —— boot 头信息
//! - `fs_shell proinfo show`     —— 解析 proinfo NV 数据
//!
//! 交互式 shell（`fs_shell boot` 不带参数）仍由 `cmd::io::cmd_imgshell` 提供。

use log::info;

use super::{Op, OpCtx, OpOutcome};
use crate::cmd::bootimg::{decompress, parse as parse_boot, parse_cpio};
use crate::core::capability::{Capability, CapabilitySet};

/// 浏览对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsTarget {
    Boot,
    Proinfo,
}

/// 只读浏览动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsAction {
    Info,
    List(String),
    Cat(String),
    Show,
}

pub struct FsShellOp {
    pub target: FsTarget,
    /// 直接给镜像文件（离线），None 时从设备分区读取
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
