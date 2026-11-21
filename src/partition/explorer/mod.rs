//! super.img filesystem browser (fs_shell)
//!
//! Lazy layered reads; never load the whole super partition into memory:
//!   LP metadata(32KB) → ext4 superblock(4KB) → BGD(64B) → inode(256B) → dir/file data blocks
//!
//! Three-layer architecture:
//!   user command → Explorer (command parsing)
//!                → ext4 read functions (care only about partition logical offsets)
//!                → LpTranslator (logical offset → super.img physical offset)
//!                → FnMut(u64,u64) callback reads

pub(crate) mod ext4;
pub(crate) mod lp;
pub(crate) mod read;
pub(crate) mod shell;
pub(crate) mod shell2;
pub(crate) mod shell3;

pub use read::{read_file_by_path, read_file_via_lp, run_explorer};
pub use shell::Explorer;

// ============================================================================
// Shared LP constants
// ============================================================================

/// LP magic values
const LP_GEOMETRY_MAGIC: u32 = 0x674C4164; // AOSP standard "gDla"
const LP_GEOMETRY_MAGIC_ALT: u32 = 0x616C4467; // this device's "gDla" variant
const LP_HEADER_MAGIC: u32 = 0x414C5030; // "0PLA"

/// LP attribute bits
const LP_ATTR_READONLY: u32 = 0x00000001;
const LP_ATTR_SLOT_SUFFIXED: u32 = 0x00000002;

// ============================================================================
// Helpers
// ============================================================================

fn fmt_size(n: u64) -> String {
    if n < 1024 {
        format!("{}B", n)
    } else if n < 1048576 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else if n < 1073741824 {
        format!("{:.1}MB", n as f64 / 1048576.0)
    } else {
        format!("{:.2}GB", n as f64 / 1073741824.0)
    }
}

fn fmt_inode_size(n: u64) -> String {
    if n < 1024 {
        format!("{}", n)
    } else if n < 1048576 {
        format!("{:.0}K", n as f64 / 1024.0)
    } else if n < 1073741824 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else {
        format!("{:.1}G", n as f64 / 1073741824.0)
    }
}

fn is_dir_mode(mode: u16) -> bool {
    (mode & 0xF000) == 0x4000
}

fn is_file_mode(mode: u16) -> bool {
    (mode & 0xF000) == 0x8000
}

fn file_type_str(ft: u8) -> &'static str {
    match ft {
        1 => "FILE",
        2 => "DIR ",
        7 => "LINK",
        _ => "????",
    }
}
