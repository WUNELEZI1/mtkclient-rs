//! 分区/GPT 处理模块
//!
//! 子模块：
//! - `gpt`     — `PartitionEntry` / `GptInfo` 数据结构与解析
//! - `io`      — `DAXFlash` 上的分区读写擦操作（read_gpt / write_partition / erase_partition）
//! - `scatter` — 从 GPT 数据生成 scatter 文件（MTK SP Flash / 刷机匣 YAML）

#[path = "gpt.rs"]
pub mod gpt;
#[path = "io.rs"]
pub mod io;
#[path = "partition_table.rs"]
pub mod partition_table;
#[path = "super.rs"]
pub mod lp;
#[path = "write.rs"]
pub(crate) mod write;

pub use gpt::GptInfo;
#[allow(unused_imports)]
pub use partition_table::{
    generate_scatter_from_gpt, generate_scatter_header, parse_gpt_from_data,
};
// 注意：read_gpt / read_partition / write_partition / write_partition_with_verify /
// erase_partition 是 DAXFlash 的方法（在 输入输出.rs 的 impl 块中定义），
// 调用方式：crate::partition::io::DAXFlash::read_gpt(&mut da)
