//! `format`: flash a minimal filesystem image into a partition (the format half of erase + format).
//!
//! Unlike `erase_data`, which always flashes f2fs into userdata, this op lets you specify
//! the partition and filesystem (f2fs / ext4), for factory-reset scenarios on traditional
//! ext4 partitions such as metadata / cache. Both image generators are pure Rust, with no
//! mke2fs/make_f2fs dependency; ext4 output has been verified mountable with e2fsck 1.47 (all 5 phases pass).
//!
//! Safety: formatting overwrites the entire partition contents, always goes through danger confirmation; magic is read back and verified after writing.

use log::info;

use super::{Op, OpCtx, OpOutcome, danger_for_partition};
use crate::core::capability::{Capability, CapabilitySet};

/// Format a partition to the specified filesystem.
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

        // Read-back verification: confirm the magic actually landed, avoiding "wrote but not applied".
        // ext4 superblock is at 1024, magic at sb+0x38 = 0x438;
        // f2fs superblock is also at 1024, magic at sb+0 = 0x400.
        let back = ctx
            .session
            .read_flash(span.addr, 0x43A.min(image.len() as u64))?;
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
                self.part,
                self.fs,
                back.len()
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
