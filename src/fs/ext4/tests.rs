//! ext4 unit tests (mechanically split from ext4.rs)

use super::*;
use crate::util::parse_u64_hex;

#[test]
fn test_create_minimal_ext4_128mb() {
    let partition_size = 128 * 1024 * 1024u64;
    let image = create_minimal_ext4(partition_size);
    // Returned structural prefix (through the end of the root dir data block): root_dir_blk=20 + 1 root dir block = 21 blocks,
    // length = 21*4096, independent of partition size. total_blocks declared in the superblock = 21 + 16 reserved free = 37.
    assert_eq!(image.len(), 21 * 4096);

    // Verify the superblock magic
    let magic = u16::from_le_bytes([image[1024 + 0x38], image[1024 + 0x39]]);
    assert_eq!(magic, EXT4_MAGIC);

    // Verify the block size logarithm
    let log_block = u32::from_le_bytes([
        image[1024 + 0x18],
        image[1024 + 0x19],
        image[1024 + 0x1A],
        image[1024 + 0x1B],
    ]);
    assert_eq!(log_block, 2);

    // Verify total_blocks declared in the superblock (including 16 reserved free blocks) = 37
    let s_blocks_count = u32::from_le_bytes([
        image[1024 + 0x04],
        image[1024 + 0x05],
        image[1024 + 0x06],
        image[1024 + 0x07],
    ]);
    assert_eq!(s_blocks_count, 37);

    // Verify the inode bitmap: inodes 1..10 are reserved (bit 0..9 = 0x3FF), all other inode bits are 0,
    // consistent with free_inodes = inodes_per_group - 10 = 246 in the superblock/GDT. The inode bitmap is in block 3.
    let inode_bitmap_lo = image[3 * 4096]; // inode bitmap in block 3
    let inode_bitmap_hi = image[3 * 4096 + 1];
    assert_eq!(
        u16::from_le_bytes([inode_bitmap_lo, inode_bitmap_hi]),
        0x3FF,
        "inode 位图低 16 位应标记 inode 1..10 已用（0x3FF），与空闲计数一致"
    );
    // Free inode count (superblock 0x10) = inodes_per_group - 10 = 246
    let s_free_inodes = u32::from_le_bytes([
        image[1024 + 0x10],
        image[1024 + 0x11],
        image[1024 + 0x12],
        image[1024 + 0x13],
    ]);
    assert_eq!(s_free_inodes, INODES_PER_GROUP - 10);

    // Verify the root dir inode (inode table in block 4)
    let inode_table_start = 4 * 4096;
    let root_inode_off = inode_table_start + 256;
    let i_mode = u16::from_le_bytes([image[root_inode_off], image[root_inode_off + 1]]);
    assert_eq!(i_mode, 0x41ED);

    // Verify the root dir data block location
    // inode_table_blocks = (256 * 256 + 4095) / 4096 = 16
    // gdt=1, block bitmap=2, inode bitmap=3, inode table=4..19, root dir=20
    // Verify the inode points to the correct data block
    let i_block0 = u32::from_le_bytes([
        image[root_inode_off + 0x28],
        image[root_inode_off + 0x29],
        image[root_inode_off + 0x2A],
        image[root_inode_off + 0x2B],
    ]);
    assert_eq!(i_block0, 20); // root_dir_blk = 20

    // Verify the root dir data block
    let dir_block_start = 20 * 4096;
    let inode = u32::from_le_bytes([
        image[dir_block_start],
        image[dir_block_start + 1],
        image[dir_block_start + 2],
        image[dir_block_start + 3],
    ]);
    assert_eq!(inode, 2); // . entry point to inode 2
    // name_len is at offset 6 (inode 4 + rec_len 2)
    assert_eq!(image[dir_block_start + 6], 1); // name_len = 1
    // name is at offset 8 (inode 4 + rec_len 2 + name_len 1 + file_type 1)
    assert_eq!(image[dir_block_start + 8], b'.'); // name = "."

    // The mtkclient-rs format path writes a raw image directly (DA streaming write, no sparse wrapper);
    // structural assertions are covered above.

    // Oversized partition check: the generator is internally consistent regardless of partition size (still only the 21-block structural prefix)
    let big = create_minimal_ext4(238u64 * 1024 * 1024 * 1024);
    assert_eq!(big.len(), 21 * 4096);
}

#[test]
fn test_parse_u64_hex_variants() {
    // With 0x prefix
    assert_eq!(parse_u64_hex("0x1A638F8000").unwrap(), 0x001A_638F_8000);
    // Hex without prefix (MediaTek LK partition-size variable format)
    assert_eq!(parse_u64_hex("1a638f8000").unwrap(), 0x001A_638F_8000);
    assert_eq!(parse_u64_hex("80000").unwrap(), 0x80000);
    // With surrounding spaces
    assert_eq!(parse_u64_hex(" 0x280000000 ").unwrap(), 0x2_8000_0000);
}
