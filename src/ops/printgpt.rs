//! `printgpt`：打印 GPT 分区表。
//!
//! 这是第一个完全走 Orchestrator 的 op：它只声明
//! `StorageRead | GptParsed`，不再自己决定"先 bypass 还是先 DA"。

use log::{debug, info};

use super::{Op, OpCtx, OpOutcome};
use crate::core::capability::{Capability, CapabilitySet};

/// 打印 GPT 分区表。
pub struct PrintGptOp;

impl PrintGptOp {
    pub fn required() -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageRead)
            .with(Capability::GptParsed)
    }
}

impl Op for PrintGptOp {
    fn name(&self) -> &'static str {
        "printgpt"
    }

    fn required_caps(&self) -> CapabilitySet {
        Self::required()
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let info = ctx.session.device_info()?;

        println!();
        println!("──────── 设备信息 ────────");
        println!(
            "  芯片: {}  hw=0x{:04X}  da=0x{:04X}  {}{}",
            if info.name.is_empty() {
                "unknown"
            } else {
                &info.name
            },
            info.hw_code,
            info.da_code,
            info.protocol,
            format!("  ({})", info.emmc_type)
        );
        if let Some(slot) = info.slot {
            println!("  活动槽   : {}", slot);
        }
        if !info.cid.is_empty() {
            let cid: String = info.cid.iter().map(|b| format!("{:02X}", b)).collect();
            println!("  CID      : {}", cid);
        }
        println!(
            "  Boot1: 0x{:08X} ({:.2} MB)   Boot2: 0x{:08X} ({:.2} MB)",
            info.boot1,
            info.boot1 as f64 / 1_048_576.0,
            info.boot2,
            info.boot2 as f64 / 1_048_576.0
        );
        println!(
            "  用户区: 0x{:08X} ({:.2} MB)   块大小: {}",
            info.user,
            info.user as f64 / 1_048_576.0,
            info.block
        );

        let gpt = ctx
            .session
            .gpt()
            .ok_or_else(|| "GPT 未解析（能力 GptParsed 未真正获得）".to_string())?
            .to_vec();
        let parsed = crate::partition::GptInfo::parse(&gpt)?;
        let count = parsed.partitions().len();

        // super 动态分区：真实设备上读 metadata，仿真设备为 None
        let sup = ctx.session.super_metadata();
        // 复用 cmd 层已有的表格渲染，保证与 `printgpt` 老命令输出一致
        crate::cmd::partition_table::print_gpt_table_public(
            &gpt,
            info.boot1,
            info.boot2,
            sup.as_ref(),
        );
        info!("{}", format!("GPT 读取成功：{} 个分区", count));

        let mut outcome = OpOutcome {
            summary: format!("GPT 已解析，{} 个分区", count),
            artifacts: Vec::new(),
            facts: vec![("partitions".into(), count.to_string())],
        };

        if ctx.write_artifacts {
            match crate::partition::generate_scatter_from_gpt(&gpt, "scatter.txt") {
                Ok(parts) => {
                    debug!("scatter.txt 已生成 ({} 分区)", parts.len());
                    outcome.artifacts.push("scatter.txt".into());
                }
                Err(e) => debug!("scatter 生成失败: {}", e),
            }
            if ctx.log_level >= 3 {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let file = format!("gpt_debug_{}.bin", ts);
                if std::fs::write(&file, &gpt).is_ok() {
                    outcome.artifacts.push(file);
                }
            }
        }

        Ok(outcome)
    }
}
