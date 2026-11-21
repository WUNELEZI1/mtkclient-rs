//! Write-side checkpoint (resume) support.
//!
//! Design points:
//! - **Backend-agnostic**: the real device (`DAXFlash`) and the simulation (`da::engine`) share one checkpoint;
//! - **Strong evidence only**: resume only if the partition name, target address, total length, source file size and mtime, and **GPT hash** all
//!   match; any change restarts from scratch (better to rewrite than to write to the wrong offset);
//! - **One CRC per 64 MiB**: verify data already written on the device before resuming; abort immediately if the prefix is corrupt,
//!   never "keep writing past it" and leave bad data in the partition;
//! - **Persistence timing**: flush every 8 MiB and immediately on error; a killed process loses at most 8 MiB;
//! - **Delete on completion**: remove `.resume` on success to avoid an accidental resume next time.

mod crc;
mod file;
mod tracker;

pub use crc::{Crc32, crc32, file_signature};
pub use file::{ChunkCrc, WriteCheckpoint};
pub use tracker::CrcTracker;

/// Checkpoint version (incremented when the format changes; old files are invalidated automatically).
///
/// v1: offset only.
/// v2: added the GPT CRC and per-chunk CRC list (Phase 1.2).
const VERSION: u32 = 2;

/// Resume state file suffix.
pub const RESUME_SUFFIX: &str = ".resume";

/// Persistence interval.
pub const PERSIST_INTERVAL: u64 = 8 * 1024 * 1024;

/// Chunk size for per-chunk CRC (64 MiB).
pub const CRC_CHUNK: u64 = 64 * 1024 * 1024;

#[cfg(test)]
mod tests;
