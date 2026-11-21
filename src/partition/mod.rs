//! Partition/GPT handling module
//!
//! Submodules:
//! - `gpt`     — `PartitionEntry` / `GptInfo` data structures and parsing
//! - `io`      — partition read/write/erase operations on `DAXFlash` (read_gpt / write_partition / erase_partition)
//! - `scatter` — generate scatter files from GPT data (MTK SP Flash / flasher YAML)
//!
//! A/B slot logic now lives in the top-level [`crate::slot`] module; this module only provides the `GptInfo` it needs.

#[path = "cow.rs"]
pub(crate) mod cow;
pub mod explorer;
#[path = "ext4.rs"]
pub(crate) mod ext4;
#[path = "gpt.rs"]
pub mod gpt;
pub mod io;
pub mod lp;
#[path = "partition_table.rs"]
pub mod partition_table;
pub(crate) mod render;
pub(crate) mod write;

pub use gpt::GptInfo;
#[allow(unused_imports)]
pub use partition_table::{generate_scatter_from_gpt, generate_scatter_header};
// Note: read_gpt / read_partition / write_partition / write_partition_with_verify /
// erase_partition are DAXFlash methods (defined in impl blocks in the io submodule);
// call them as: crate::partition::io::DAXFlash::read_gpt(&mut da)
