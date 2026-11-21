//! `format`：给分区刷入最小文件系统镜像（erase + format 的 format 半步）。
//!
//! 与 `erase_data` 里固定给 userdata 刷 f2fs 不同，本 op 允许显式指定
//! 分区与文件系统（f2fs / ext4），用于 metadata / cache 等传统 ext4 分区的
//! 恢复出厂场景。两种镜像生成器都是纯 Rust 自实现，无 mke2fs/make_f2fs 依赖；
//! ext4 输出已用 e2fsck 1.47 验证可挂载（5 阶段全过）。
//!
//! 安全：格式化会覆盖分区全部内容，一律走危险确认；写入后回读 magic 校验。

use log::info;

use super::{Op, OpCtx, OpOutcome, danger_for_partition};
use crate::core::capability::{Capability, CapabilitySet};

/// 给分区格式化为指定文件系统。
pub struct FormatOp {
    pub part: String,
    /// "f2fs" / "ext4"
    pub fs: String,
}

impl Op for FormatOp {
    fn name(&self) -> &'static str {
        "format"
    }

    fn required_caps(&self) -> CapabilitySet {
        CapabilitySet::EMPTY
            .with(Capability::StorageWrite)
            .with(Capability::GptParsed)
    }

    fn run(&self, ctx: &mut OpCtx<'_>) -> Result<OpOutcome, String> {
        let span = ctx.session.find_partition(&self.part)?;
        if span.size == 0 {
            return Err(format!("分区 {} 大小为 0，无法格式化", self.part));
        }

        let image = crate::fs::format_image_as(&self.fs, span.size)?;

        ctx.confirm_action(
            "format",
            danger_for_partition(&self.part),
            &format!(
                "格式化 {} 为 {}（{} 字节镜像写入 {} 字节分区，原数据不可恢复）",
                self.part,
                self.fs,
                image.len(),
                span.size
            ),
        )?;

        info!(
            "格式化 {} 为 {}: 镜像 {} 字节 @ 0x{:X}",
            self.part,
            self.fs,
            image.len(),
            span.addr
        );
        ctx.session.write_flash(span.addr, &image)?;

        // 回读校验：确认 magic 落盘，避免"写了但没生效"。
        // ext4 superblock 在 1024，magic 在 sb+0x38 = 0x438；
        // f2fs superblock 也在 1024，magic 在 sb+0 = 0x400。
        let back = ctx.session.read_flash(span.addr, 0x43A.min(image.len() as u64))?;
        let magic_ok = match self.fs.to_ascii_lowercase().as_str() {
            "ext4" => {
                back.len() > 0x439 && u16::from_le_bytes([back[0x438], back[0x439]]) == 0xEF53
            }
            "f2fs" => {
                back.len() >= 0x404
                    && u32::from_le_bytes(back[0x400..0x404].try_into().unwrap()) == 0xF2F5_2010
            }
            _ => false,
        };
        if !magic_ok {
            return Err(format!(
                "回读校验失败: {} 的 {} magic 未正确落盘（back.len={}）",
                self.part, self.fs, back.len()
            ));
        }

        Ok(OpOutcome {
            summary: format!("{} 已格式化为 {}", self.part, self.fs),
            artifacts: Vec::new(),
            facts: vec![
                ("partition".into(), self.part.clone()),
                ("fs".into(), self.fs.clone()),
                ("image_bytes".into(), image.len().to_string()),
            ],
        })
    }
}
