//! 最小 ext4 文件系统镜像生成
//!
//! 生成一个可被 Linux 内核挂载的最小 ext4 文件系统镜像，
//! 包含有效的 superblock、group descriptors、inode table 和根目录。
//! 支持 4096 字节块大小，单个 block group。
//!
//! ## 设计约束（format 大分区，如 userdata 上百 GB）
//!
//! 调用方 [`crate::sparse::SparseImage::write_partition_sparse`] 把本结构前缀封装为 sparse 时，
//! 传入的 `total_size` 为 `prefix.len()`（**只刷结构块，不覆盖整个分区**）——剩余区域由 format 前的
//! `erase` 清空为零、处于文件系统之外。因此本生成器产出的文件系统**只声明其自身结构所占的块数**
//! （约 20 块 ≈ 80KB），而非整个分区大小；这与官方 `make_ext4fs` 把 fs 铺满分区的做法不同，却是
//! 我们「erase + 只写结构」方案下**内部自洽**的唯一正确做法（否则 `total_blocks` 远超实际写入的
//! 单 group 结构，会触发内核拒绝挂载）。
//!
//! 由于结构块数恒远小于 `BLOCKS_PER_GROUP`（32768 → 128MB），单个 block group 即可完整描述任意
//! 分区大小的文件系统，无需多 group 布局，也无需为整个分区分配内存。

/// ext4 魔数
const EXT4_MAGIC: u16 = 0xEF53;

/// 默认块大小
const BLOCK_SIZE: u64 = 4096;

/// 块大小的对数（1024 << 2 = 4096）
const LOG_BLOCK_SIZE: u32 = 2;

/// 每个 block group 的块数（标准值；结构块数恒小于它，故始终单 group）
const BLOCKS_PER_GROUP: u32 = 32768;

/// 每个 block group 的 inode 数
const INODES_PER_GROUP: u32 = 256;

/// inode 大小
const INODE_SIZE: u16 = 256;

/// 根目录 inode 编号
const ROOT_INODE: u32 = 2;

/// 文件类型: 目录
const FILE_TYPE_DIR: u8 = 2;

/// 默认卷标
const VOLUME_LABEL: &[u8] = b"fastboot-rs";

/// 生成最小 ext4 文件系统的**结构前缀**（superblock / GDT / bitmap / inode table / 根目录数据块）。
///
/// 只分配「结构区」大小的内存（`used_blocks` 个块，约 80KB），**不**为整个分区分配 `partition_size`
/// 的内存——这是 format 大分区（如 userdata 可达上百 GB）时的关键约束：否则 `vec![0u8; partition_size]`
/// 会因分配整块分区镜像而 OOM / 极慢。
///
/// 返回的 `Vec<u8>` 为结构前缀，`prefix.len()` 为 `BLOCK_SIZE` 的整数倍；文件系统自身声明的
/// `total_blocks` 恰为该结构块数（约 20 块），剩余到分区末尾的区域全为零、处于文件系统之外。
///
/// `partition_size`: 分区大小（字节），仅用于最低合法性校验（分区必须至少能容纳结构区）。
pub fn create_minimal_ext4(partition_size: u64) -> Vec<u8> {
    // 4096 字节块大小下，单个 block group 的元数据结构固定占用 21 个块（84KB）：
    //   block 0      : superblock（@1024）
    //   block 1      : Group Descriptor Table（GDT）
    //   block 2      : block bitmap
    //   block 3      : inode bitmap
    //   block 4..19  : inode table（16 块）
    //   block 20     : 根目录数据块
    // 要求分区至少能容纳结构区（留少量余量）。
    assert!(
        partition_size >= 21 * BLOCK_SIZE,
        "分区太小，无法容纳 ext4 结构区（至少需 {} 字节）",
        21 * BLOCK_SIZE
    );

    let inodes_per_group = INODES_PER_GROUP;

    // 计算 inode table 占用的块数
    let inode_table_blocks = (inodes_per_group as u64 * INODE_SIZE as u64).div_ceil(BLOCK_SIZE); // 16

    // 块号布局（4096 字节块，block 编号从 0 起）。旧实现把 GDT 写在偏移 2048（那是 1KiB 块布局下
    // 的位置），导致内核 / e2fsck 按 4096 块布局在 block 1 处读到的是 bitmap 而非 GDT，报
    // "Corrupt group descriptor: bad block for block bitmap"。此处严格按 4096 块布局顺移一块。
    let gdt_blk: u32 = 1;
    let block_bitmap_blk: u32 = 2;
    let inode_bitmap_blk: u32 = 3;
    let inode_table_blk: u32 = 4;
    let root_dir_blk = inode_table_blk + inode_table_blocks as u32; // 20
    let used_blocks = root_dir_blk + 1; // 21

    // 文件系统声明的块数 = 已用结构块 + 一小段保留空闲块（避免「0 空闲」导致挂载后被视为满盘）。
    // 这些保留块位于结构区之后、处于文件系统之内但未被任何元数据/数据占用，磁盘上为零（format 前
    // 的 erase 已清零），语义等价于「空闲块」。
    let free_slack: u32 = 16;
    let total_blocks = used_blocks + free_slack; // 37

    // 只分配结构区（到根目录数据块末尾），避免为整个分区分配内存
    let structure_len = (used_blocks as u64 * BLOCK_SIZE) as usize;
    let mut image = vec![0u8; structure_len];

    // ==================== Superblock (偏移 1024 字节) ====================
    let sb = &mut image[1024..2048];

    // s_inodes_count (0x00)
    write_u32_le(sb, 0x00, inodes_per_group);
    // s_blocks_count_lo (0x04)
    write_u32_le(sb, 0x04, total_blocks);
    // s_r_blocks_count_lo (0x08) - 保留块数
    write_u32_le(sb, 0x08, 0);
    // s_free_blocks_count_lo (0x0C)
    write_u32_le(sb, 0x0C, total_blocks - used_blocks);
    // s_free_inodes_count (0x10): inode 1..10 为保留 inode（已用），故空闲 = 256 - 10 = 246。
    write_u32_le(sb, 0x10, inodes_per_group - 10);
    // s_first_data_block (0x14) - 对于 4096 块大小，为 0
    write_u32_le(sb, 0x14, 0);
    // s_log_block_size (0x18) - 4096 = 1024 << 2
    write_u32_le(sb, 0x18, LOG_BLOCK_SIZE);
    // s_log_cluster_size (0x1C) - 同块大小
    write_u32_le(sb, 0x1C, LOG_BLOCK_SIZE);
    // s_blocks_per_group (0x20)
    write_u32_le(sb, 0x20, BLOCKS_PER_GROUP);
    // s_clusters_per_group (0x24) - 同 blocks_per_group
    write_u32_le(sb, 0x24, BLOCKS_PER_GROUP);
    // s_inodes_per_group (0x28)
    write_u32_le(sb, 0x28, inodes_per_group);
    // s_mtime (0x2C) - 挂载时间
    write_u32_le(sb, 0x2C, 0);
    // s_wtime (0x30) - 写入时间
    write_u32_le(sb, 0x30, 0);
    // s_mnt_count (0x34)
    write_u16_le(sb, 0x34, 0);
    // s_max_mnt_count (0x36) - -1 表示不检查
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

    // s_first_ino (0x54) - 第一个非保留 inode
    write_u32_le(sb, 0x54, 11);
    // s_inode_size (0x58)
    write_u16_le(sb, 0x58, INODE_SIZE);
    // s_block_group_nr (0x5A) - 当前正在操作的 block group
    write_u16_le(sb, 0x5A, 0);
    // s_feature_compat (0x5C)
    write_u32_le(sb, 0x5C, 0);
    // s_feature_incompat (0x60): 仅 FILETYPE(0x2)。
    // 旧代码写 0x42 并称其为 FLEX_BG，但 0x40 实为 INCOMPAT_EXTENTS；且根目录 inode 使用的是传统
    // i_block[] 直接块映射、i_flags=0、无 extent tree 头（magic 0xF30A），与 EXTENTS 声明矛盾。
    // 去掉误声明的位，仅保留目录项携带文件类型（被根目录条目用到）。
    write_u32_le(sb, 0x60, 0x2);
    // s_feature_ro_compat (0x64)
    write_u32_le(sb, 0x64, 0);
    // s_uuid (0x68) - 16 字节 UUID
    // 使用固定 UUID 以便可重复
    let uuid: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    sb[0x68..0x78].copy_from_slice(&uuid);
    // s_volume_name (0x78) - 16 字节
    let label_len = VOLUME_LABEL.len().min(16);
    sb[0x78..0x78 + label_len].copy_from_slice(&VOLUME_LABEL[..label_len]);

    // s_last_mounted (0x88) - 64 字节
    // 保持为零

    // s_algorithm_usage_bitmap (0xC8) - 4 字节
    write_u32_le(sb, 0xC8, 0);

    // s_prealloc_blocks (0xCC)
    write_u8(sb, 0xCC, 0);
    // s_prealloc_dir_blocks (0xCD)
    write_u8(sb, 0xCD, 0);

    // s_reserved_gdt_blocks (0xCE) - 2 字节
    write_u16_le(sb, 0xCE, 0);

    // s_journal_uuid (0xD0) - 16 字节
    // 保持为零（无日志）

    // s_journal_inum (0xE0)
    write_u32_le(sb, 0xE0, 0);
    // s_journal_dev (0xE4)
    write_u32_le(sb, 0xE4, 0);
    // s_last_orphan (0xE8)
    write_u32_le(sb, 0xE8, 0);

    // s_hash_seed (0xEC) - 16 字节
    // 保持为零

    // s_def_hash_version (0xFC)
    write_u8(sb, 0xFC, 0);
    // s_reserved_char_pad (0xFD)
    write_u8(sb, 0xFD, 0);

    // s_default_mount_opts (0xFE) - 4 字节
    write_u32_le(sb, 0xFE, 0);

    // s_first_meta_bg (0x102) - 4 字节
    write_u32_le(sb, 0x102, 0);

    // s_mkfs_time (0x106) - 4 字节
    write_u32_le(sb, 0x106, 0);

    // s_jnl_blocks (0x10A) - 17 * 4 字节
    // 保持为零

    // ==================== Group Descriptors (block 1, 偏移 4096) ====================
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
    // bg_free_inodes_count_lo (0x0E): 同 s_free_inodes_count = 246
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
    // bg_itable_unused_lo (0x1C): 已用 inode 数 = 保留 10（inode 1..10，含根目录 inode 2）+ 无额外
    // 用户 inode；与 s_free_inodes_count(246) 自洽。
    write_u16_le(gdt, 0x1C, (inodes_per_group - 10) as u16);
    // bg_checksum (0x1E)
    write_u16_le(gdt, 0x1E, 0);

    // ==================== Block Bitmap (block 2) ====================
    let block_bitmap_start = (block_bitmap_blk as usize) * BLOCK_SIZE as usize;
    // 标记前 used_blocks 个块为已用 (1)；block 0(superblock)..block 20(根目录) 全部已用。
    for i in 0..used_blocks as usize {
        let byte_idx = block_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }
    // 每组边界外（[total_blocks, s_blocks_per_group)）的位必须置 1，否则 e2fsck 报
    // "Padding at end of block bitmap is not set"。
    for i in total_blocks as usize..BLOCKS_PER_GROUP as usize {
        let byte_idx = block_bitmap_start + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }

    // ==================== Inode Bitmap (block 3) ====================
    let inode_bitmap_start = (inode_bitmap_blk as usize) * BLOCK_SIZE as usize;
    // inode 编号从 1 开始；inode 1..10 为保留 inode，必须标记为已用（bit 0..9），其余未用 inode
    // 的位保持 0。这样 s_free_inodes_count = 256 - 10 = 246 与位图完全一致（不再是「只有 inode 2」）。
    // 超出 s_inodes_per_group 的位（padding）置 1，与 block bitmap 同口径。
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

    // ==================== Inode Table (block 4 开始) ====================
    let inode_table_start = (inode_table_blk as usize) * BLOCK_SIZE as usize;

    // 每个 inode 256 字节，inode 编号从 1 开始
    // inode 1: bad block inode (保留，可保持为零)
    // inode 2: root directory inode

    // 根目录 inode 偏移: inode_table_start + (2 - 1) * INODE_SIZE
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
    // i_block[0] (0x28): 指向根目录数据块 20
    write_u32_le(root_inode, 0x28, root_dir_blk);
    // i_block[1..15] 保持为零
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
    // i_extra_isize (0x76): 扩展 inode 大小
    write_u16_le(root_inode, 0x76, 0);

    // ==================== 根目录数据块 (block root_dir_blk = 20) ====================
    let dir_block_start = (root_dir_blk as u64 * BLOCK_SIZE) as usize;
    let dir_block = &mut image[dir_block_start..dir_block_start + BLOCK_SIZE as usize];

    let mut offset = 0usize;

    // 条目 ".": inode=2, type=dir
    // ext4_dir_entry: inode(4) + rec_len(2) + name_len(1) + file_type(1) + name(1)
    let entry1_rec_len = 12u16; // 4+2+1+1+1 = 9，按 4 字节对齐到 12
    write_u32_le(dir_block, offset, ROOT_INODE);
    offset += 4;
    write_u16_le(dir_block, offset, entry1_rec_len);
    offset += 2;
    write_u8(dir_block, offset, 1); // name_len = 1
    offset += 1;
    write_u8(dir_block, offset, FILE_TYPE_DIR); // file_type = 2 (dir)
    offset += 1;
    dir_block[offset] = b'.';
    // 条目以 rec_len 对齐：下一个条目必须落在 entry1_rec_len 边界（12），中间 padding 保持 0。
    offset = entry1_rec_len as usize;

    // 条目 "..": inode=2, type=dir，最后一个条目填满剩余空间（rec_len = 块大小 - 12）
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
    // 剩余空间保持为零

    image
}

/// 计算 superblock 的校验和（可用于验证）
#[allow(dead_code)]
fn superblock_checksum(_sb: &[u8]) -> u32 {
    // 简单的 CRC-32C 风格校验，但为了最小实现，返回 0
    // 实际 ext4 使用 CRC-32C
    0
}

// ==================== 小端写入辅助函数 ====================

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
