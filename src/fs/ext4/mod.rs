//! Minimal ext4 filesystem image generation.
//!
//! Generates a minimal ext4 image mountable by the Linux kernel, containing a valid
//! superblock, group descriptors, inode table and root directory. Supports a 4096-byte
//! block size and a single block group.
//!
//! ## Design constraint (formatting large partitions such as userdata of 100+ GB)
//!
//! When the caller `crate::sparse::SparseImage::write_partition_sparse` wraps this
//! structure prefix into a sparse image, the `total_size` passed in is `prefix.len()`
//! (**only structure blocks are flashed, never the whole partition**) - the remaining
//! area has been zeroed by the `erase` before format and lies outside the filesystem.
//! The filesystem produced here therefore **declares only the block count occupied by
//! its own structures** (about 20 blocks, ~80KB), not the whole partition size. This
//! differs from official `make_ext4fs`, which spreads the fs over the whole partition,
//! but it is the only **internally consistent** choice for our "erase + write structure
//! only" scheme (otherwise `total_blocks` would far exceed the single-group structure
//! actually written and the kernel would refuse to mount).
//!
//! Since the structure block count is always far below `BLOCKS_PER_GROUP`
//! (32768 -> 128MB), a single block group fully describes a filesystem of any partition
//! size, with no multi-group layout and no need to allocate memory for the whole partition.

/// ext4 magic
const EXT4_MAGIC: u16 = 0xEF53;

/// Default block size
const BLOCK_SIZE: u64 = 4096;

/// Log2 of the block size (1024 << 2 = 4096)
const LOG_BLOCK_SIZE: u32 = 2;

/// Blocks per block group (standard value; structure blocks are always fewer, so a single group always suffices)
const BLOCKS_PER_GROUP: u32 = 32768;

/// Inodes per block group
const INODES_PER_GROUP: u32 = 256;

/// Inode size
const INODE_SIZE: u16 = 256;

/// Root directory inode number
const ROOT_INODE: u32 = 2;

/// File type: directory
const FILE_TYPE_DIR: u8 = 2;

/// Default volume label
const VOLUME_LABEL: &[u8] = b"fastboot-rs";

/// Generates the **structure prefix** of a minimal ext4 filesystem (superblock / GDT /
/// bitmaps / inode table / root directory data block).
///
/// Allocates memory only for the "structure area" (`used_blocks` blocks, ~80KB) and
/// **not** `partition_size` bytes for the whole partition - the key constraint when
/// formatting large partitions (userdata can reach 100+ GB), otherwise
/// `vec![0u8; partition_size]` would OOM / be extremely slow from allocating a full
/// partition image.
///
/// The returned `Vec<u8>` is the structure prefix and `prefix.len()` is a multiple of
/// `BLOCK_SIZE`; the filesystem's declared `total_blocks` is exactly that structure
/// block count (~20 blocks), and everything from there to the end of the partition is
/// zero and outside the filesystem.
///
/// `partition_size`: partition size in bytes, used only for the minimum validity check
/// (the partition must at least hold the structure area).
pub fn create_minimal_ext4(partition_size: u64) -> Vec<u8> {
    // With a 4096-byte block size, a single block group's metadata structures occupy
    // exactly 21 blocks (84KB):
    //   block 0      : superblock (@1024)
    //   block 1      : Group Descriptor Table (GDT)
    //   block 2      : block bitmap
    //   block 3      : inode bitmap
    //   block 4..19  : inode table (16 blocks)
    //   block 20     : root directory data block
    // The partition must at least hold the structure area (with a small margin).
    assert!(
        partition_size >= 21 * BLOCK_SIZE,
        "分区太小，无法容纳 ext4 结构区（至少需 {} 字节）",
        21 * BLOCK_SIZE
    );

    let inodes_per_group = INODES_PER_GROUP;

    // Number of blocks occupied by the inode table
    let inode_table_blocks = (inodes_per_group as u64 * INODE_SIZE as u64).div_ceil(BLOCK_SIZE); // 16

    // Block number layout (4096-byte blocks, block numbering from 0). The old
    // implementation wrote the GDT at offset 2048 (the position under a 1KiB block
    // layout), so the kernel / e2fsck, using a 4096 block layout, read the bitmap at
    // block 1 instead of the GDT and reported
    // "Corrupt group descriptor: bad block for block bitmap". Here the layout follows
    // 4096-byte blocks strictly, shifted by one block.
    let gdt_blk: u32 = 1;
    let block_bitmap_blk: u32 = 2;
    let inode_bitmap_blk: u32 = 3;
    let inode_table_blk: u32 = 4;
    let root_dir_blk = inode_table_blk + inode_table_blocks as u32; // 20
    let used_blocks = root_dir_blk + 1; // 21

    // Blocks declared by the filesystem = used structure blocks + a small pool of reserved
    // free blocks (to avoid "0 free", which makes the fs look full after mounting). These
    // reserved blocks follow the structure area, lie inside the filesystem but are used by
    // no metadata/data, and are zero on disk (erased before format), so semantically they
    // equal "free blocks".
    let free_slack: u32 = 16;
    let total_blocks = used_blocks + free_slack; // 37

    // Allocate only the structure area (up to the end of the root directory data block), avoiding memory allocation for the whole partition
    let structure_len = (used_blocks as u64 * BLOCK_SIZE) as usize;
    let mut image = vec![0u8; structure_len];

    // ==================== Superblock (offset 1024 bytes) ====================
    let sb = &mut image[1024..2048];

    // s_inodes_count (0x00)
    write_u32_le(sb, 0x00, inodes_per_group);
    // s_blocks_count_lo (0x04)
    write_u32_le(sb, 0x04, total_blocks);
    // s_r_blocks_count_lo (0x08) - reserved block count
    write_u32_le(sb, 0x08, 0);
    // s_free_blocks_count_lo (0x0C)
    write_u32_le(sb, 0x0C, total_blocks - used_blocks);
    // s_free_inodes_count (0x10): inodes 1..10 are reserved (in use), so free = 256 - 10 = 246.
    write_u32_le(sb, 0x10, inodes_per_group - 10);
    // s_first_data_block (0x14) - 0 for a 4096-byte block size
    write_u32_le(sb, 0x14, 0);
    // s_log_block_size (0x18) - 4096 = 1024 << 2
    write_u32_le(sb, 0x18, LOG_BLOCK_SIZE);
    // s_log_cluster_size (0x1C) - same as block size
    write_u32_le(sb, 0x1C, LOG_BLOCK_SIZE);
    // s_blocks_per_group (0x20)
    write_u32_le(sb, 0x20, BLOCKS_PER_GROUP);
    // s_clusters_per_group (0x24) - same as blocks_per_group
    write_u32_le(sb, 0x24, BLOCKS_PER_GROUP);
    // s_inodes_per_group (0x28)
    write_u32_le(sb, 0x28, inodes_per_group);
    // s_mtime (0x2C) - mount time
    write_u32_le(sb, 0x2C, 0);
    // s_wtime (0x30) - write time
    write_u32_le(sb, 0x30, 0);
    // s_mnt_count (0x34)
    write_u16_le(sb, 0x34, 0);
    // s_max_mnt_count (0x36) - -1 means never check
    write_u16_le(sb, 0x36, 0xFFFF);
    // s_magic (0x38)
    write_u16_le(sb, 0x38, EXT4_MAGIC);
    // s_state (0x3A) - 1 = clean
    write_u16_le(sb, 0x3A, 1);
    // s_errors (0x3C) - 1 = continue
    write_u16_le(sb, 0x3C, 1);
    // s_minor_rev_level (0x3E)
    write_u16_le(sb, 0x3E, 0);
    // s_lastcheck (0x40)
    write_u32_le(sb, 0x40, 0);
    // s_checkinterval (0x44)
    write_u32_le(sb, 0x44, 0);
    // s_creator_os (0x48) - 0 = Linux
    write_u32_le(sb, 0x48, 0);
    // s_rev_level (0x4C) - 1 = dynamic
    write_u32_le(sb, 0x4C, 1);
    // s_def_resuid (0x50)
    write_u16_le(sb, 0x50, 0);
    // s_def_resgid (0x52)
    write_u16_le(sb, 0x52, 0);

    // s_first_ino (0x54) - first non-reserved inode
    write_u32_le(sb, 0x54, 11);
    // s_inode_size (0x58)
    write_u16_le(sb, 0x58, INODE_SIZE);
    // s_block_group_nr (0x5A) - block group currently being operated on
    write_u16_le(sb, 0x5A, 0);
    // s_feature_compat (0x5C)
    write_u32_le(sb, 0x5C, 0);
    // s_feature_incompat (0x60): FILETYPE (0x2) only.
    // The old code wrote 0x42 and called it FLEX_BG, but 0x40 is actually INCOMPAT_EXTENTS; moreover the root inode uses the traditional
    // i_block[] direct block mapping, i_flags=0, and no extent tree header (magic 0xF30A), contradicting the EXTENTS claim.
    // Drop the wrongly declared bit and keep only FILETYPE (directory entries carry the file type, used by the root entries).
    write_u32_le(sb, 0x60, 0x2);
    // s_feature_ro_compat (0x64)
    write_u32_le(sb, 0x64, 0);
    // s_uuid (0x68) - 16-byte UUID
    // Use a fixed UUID for reproducibility
    let uuid: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    sb[0x68..0x78].copy_from_slice(&uuid);
    // s_volume_name (0x78) - 16 bytes
    let label_len = VOLUME_LABEL.len().min(16);
    sb[0x78..0x78 + label_len].copy_from_slice(&VOLUME_LABEL[..label_len]);

    // s_last_mounted (0x88) - 64 bytes
    // Keep zero

    // s_algorithm_usage_bitmap (0xC8) - 4 bytes
    write_u32_le(sb, 0xC8, 0);

    // s_prealloc_blocks (0xCC)
    write_u8(sb, 0xCC, 0);
    // s_prealloc_dir_blocks (0xCD)
    write_u8(sb, 0xCD, 0);

    // s_reserved_gdt_blocks (0xCE) - 2 bytes
    write_u16_le(sb, 0xCE, 0);

    // s_journal_uuid (0xD0) - 16 bytes
    // Keep zero (no journal)

    // s_journal_inum (0xE0)
    write_u32_le(sb, 0xE0, 0);
    // s_journal_dev (0xE4)
    write_u32_le(sb, 0xE4, 0);
    // s_last_orphan (0xE8)
    write_u32_le(sb, 0xE8, 0);

    // s_hash_seed (0xEC) - 16 bytes
    // Keep zero

    // s_def_hash_version (0xFC)
    write_u8(sb, 0xFC, 0);
    // s_reserved_char_pad (0xFD)
    write_u8(sb, 0xFD, 0);

    // s_default_mount_opts (0xFE) - 4 bytes
    write_u32_le(sb, 0xFE, 0);

    // s_first_meta_bg (0x102) - 4 bytes
    write_u32_le(sb, 0x102, 0);

    // s_mkfs_time (0x106) - 4 bytes
    write_u32_le(sb, 0x106, 0);

    // s_jnl_blocks (0x10A) - 17 * 4 bytes
    // Keep zero

    // ==================== Group Descriptors (block 1, offset 4096) ====================
    let gdt = &mut image
        [(gdt_blk as usize) * BLOCK_SIZE as usize..(gdt_blk as usize) * BLOCK_SIZE as usize + 32];

    // bg_block_bitmap_lo (0x00)
    write_u32_le(gdt, 0x00, block_bitmap_blk);
    // bg_inode_bitmap_lo (0x04)
    write_u32_le(gdt, 0x04, inode_bitmap_blk);
    // bg_inode_table_lo (0x08)
    write_u32_le(gdt, 0x08, inode_table_blk);
    // bg_free_blocks_count_lo (0x0C)
    write_u16_le(gdt, 0x0C, (total_blocks - used_blocks) as u16);
    // bg_free_inodes_count_lo (0x0E): same as s_free_inodes_count = 246
    write_u16_le(gdt, 0x0E, (inodes_per_group - 10) as u16);
    // bg_used_dirs_count_lo (0x10)
    write_u16_le(gdt, 0x10, 1);
    // bg_flags (0x12)
    write_u16_le(gdt, 0x12, 0);
    // bg_exclude_bitmap_lo (0x14)
    write_u32_le(gdt, 0x14, 0);
    // bg_block_bitmap_csum_lo (0x18)
    write_u16_le(gdt, 0x18, 0);
    // bg_inode_bitmap_csum_lo (0x1A)
    write_u16_le(gdt, 0x1A, 0);
    // bg_itable_unused_lo (0x1C): used inodes = 10 reserved (inodes 1..10, including root inode 2) + no extra
    // user inodes; consistent with s_free_inodes_count(246).
    write_u16_le(gdt, 0x1C, (inodes_per_group - 10) as u16);
    // bg_checksum (0x1E)
    write_u16_le(gdt, 0x1E, 0);

    // ==================== Block Bitmap (block 2) ====================
    let block_bitmap_start = (block_bitmap_blk as usize) * BLOCK_SIZE as usize;
    // Mark the first used_blocks blocks as used (1); block 0 (superblock) .. block 20 (root directory) are all used.
    for i in 0..used_blocks as usize {
        let byte_idx = block_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }
    // Bits beyond the group boundary ([total_blocks, s_blocks_per_group)) must be set to 1, otherwise e2fsck reports
    // "Padding at end of block bitmap is not set".
    for i in total_blocks as usize..BLOCKS_PER_GROUP as usize {
        let byte_idx = block_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }

    // ==================== Inode Bitmap (block 3) ====================
    let inode_bitmap_start = (inode_bitmap_blk as usize) * BLOCK_SIZE as usize;
    // Inode numbering starts at 1; inodes 1..10 are reserved and must be marked used (bits 0..9), unused inodes
    // keep their bits 0. This makes s_free_inodes_count = 256 - 10 = 246 fully consistent with the bitmap (no longer "only inode 2").
    // Bits beyond s_inodes_per_group (padding) are set to 1, same convention as the block bitmap.
    for i in 0..10 {
        let byte_idx = inode_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }
    for i in INODES_PER_GROUP as usize..BLOCKS_PER_GROUP as usize {
        let byte_idx = inode_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }

    // ==================== Inode Table (starting at block 4) ====================
    let inode_table_start = (inode_table_blk as usize) * BLOCK_SIZE as usize;

    // Each inode is 256 bytes; inode numbering starts at 1
    // inode 1: bad block inode (reserved, may remain zero)
    // inode 2: root directory inode

    // Root directory inode offset: inode_table_start + (2 - 1) * INODE_SIZE
    let root_inode_offset = inode_table_start + (ROOT_INODE as usize - 1) * INODE_SIZE as usize;
    let root_inode = &mut image[root_inode_offset..root_inode_offset + INODE_SIZE as usize];

    // i_mode (0x00): S_IFDIR | 0755 = 0x41ED
    write_u16_le(root_inode, 0x00, 0x41ED);
    // i_uid (0x02)
    write_u16_le(root_inode, 0x02, 0);
    // i_size_lo (0x04): 1 block = 4096 bytes
    write_u32_le(root_inode, 0x04, BLOCK_SIZE as u32);
    // i_atime (0x08)
    write_u32_le(root_inode, 0x08, 0);
    // i_ctime (0x0C)
    write_u32_le(root_inode, 0x0C, 0);
    // i_mtime (0x10)
    write_u32_le(root_inode, 0x10, 0);
    // i_dtime (0x14)
    write_u32_le(root_inode, 0x14, 0);
    // i_gid (0x18)
    write_u16_le(root_inode, 0x18, 0);
    // i_links_count (0x1A): 2 (for "." and "..")
    write_u16_le(root_inode, 0x1A, 2);
    // i_blocks_lo (0x1C): 1 block = 8 sectors (512 bytes each)
    write_u32_le(root_inode, 0x1C, 8);
    // i_flags (0x20)
    write_u32_le(root_inode, 0x20, 0);
    // i_osd1 (0x24)
    write_u32_le(root_inode, 0x24, 0);
    // i_block`0` (0x28): points to root directory data block 20
    write_u32_le(root_inode, 0x28, root_dir_blk);
    // i_block[1..15] remain zero
    // i_generation (0x64)
    write_u32_le(root_inode, 0x64, 0);
    // i_file_acl (0x68)
    write_u32_le(root_inode, 0x68, 0);
    // i_size_high (0x6C)
    write_u32_le(root_inode, 0x6C, 0);
    // i_obso_faddr (0x70)
    write_u32_le(root_inode, 0x70, 0);
    // i_osd2 (0x74)
    write_u16_le(root_inode, 0x74, 0);
    // i_extra_isize (0x76): extended inode size
    write_u16_le(root_inode, 0x76, 0);

    // ==================== Root directory data block (block root_dir_blk = 20) ====================
    let dir_block_start = (root_dir_blk as u64 * BLOCK_SIZE) as usize;
    let dir_block = &mut image[dir_block_start..dir_block_start + BLOCK_SIZE as usize];

    let mut offset = 0usize;

    // Entry ".": inode=2, type=dir
    // ext4_dir_entry: inode(4) + rec_len(2) + name_len(1) + file_type(1) + name(1)
    let entry1_rec_len = 12u16; // 4+2+1+1+1 = 9, aligned to 4 bytes -> 12
    write_u32_le(dir_block, offset, ROOT_INODE);
    offset += 4;
    write_u16_le(dir_block, offset, entry1_rec_len);
    offset += 2;
    write_u8(dir_block, offset, 1); // name_len = 1
    offset += 1;
    write_u8(dir_block, offset, FILE_TYPE_DIR); // file_type = 2 (dir)
    offset += 1;
    dir_block[offset] = b'.';
    // Entries are aligned by rec_len: the next entry must land on the entry1_rec_len boundary (12), with padding kept 0.
    offset = entry1_rec_len as usize;

    // Entry "..": inode=2, type=dir; the last entry fills the remaining space (rec_len = block size - 12)
    let entry2_rec_len = (BLOCK_SIZE as u16) - entry1_rec_len;
    write_u32_le(dir_block, offset, ROOT_INODE);
    offset += 4;
    write_u16_le(dir_block, offset, entry2_rec_len);
    offset += 2;
    write_u8(dir_block, offset, 2); // name_len = 2
    offset += 1;
    write_u8(dir_block, offset, FILE_TYPE_DIR); // file_type = 2 (dir)
    offset += 1;
    dir_block[offset] = b'.';
    dir_block[offset + 1] = b'.';
    // Keep remaining space zero

    image
}

/// Compute the superblock checksum (for verification)
#[allow(dead_code)]
fn superblock_checksum(_sb: &[u8]) -> u32 {
    // A simple CRC-32C-style check, but returns 0 for minimal implementation
    // Real ext4 uses CRC-32C
    0
}

// ==================== Little-endian write helpers ====================

fn write_u8(buf: &mut [u8], offset: usize, val: u8) {
    buf[offset] = val;
}

fn write_u16_le(buf: &mut [u8], offset: usize, val: u16) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 2].copy_from_slice(&bytes);
}

fn write_u32_le(buf: &mut [u8], offset: usize, val: u32) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 4].copy_from_slice(&bytes);
}

#[cfg(test)]
mod tests;
