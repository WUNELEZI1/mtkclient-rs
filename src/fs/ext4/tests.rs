//! ext4 单元测试（从 ext4.rs 机械拆分）

use super::*;
use crate::util::parse_u64_hex;


#[test]
fn test_create_minimal_ext4_128mb() {
    let partition_size = 128 * 1024 * 1024u64;
    let image = create_minimal_ext4(partition_size);
    // 返回结构前缀（到根目录数据块末尾）：root_dir_blk=20 + 根目录块 1 = 21 块，
    // 长度 = 21*4096，不依赖分区大小。superblock 内声明的 total_blocks = 21 + 16 保留空闲 = 37。
    assert_eq!(image.len(), 21 * 4096);

    // 验证 superblock magic
    let magic = u16::from_le_bytes([image[1024 + 0x38], image[1024 + 0x39]]);
    assert_eq!(magic, EXT4_MAGIC);

    // 验证块大小对数
    let log_block = u32::from_le_bytes([
        image[1024 + 0x18],
        image[1024 + 0x19],
        image[1024 + 0x1A],
        image[1024 + 0x1B],
    ]);
    assert_eq!(log_block, 2);

    // 验证 superblock 声明的 total_blocks（含 16 块保留空闲）= 37
    let s_blocks_count = u32::from_le_bytes([
        image[1024 + 0x04],
        image[1024 + 0x05],
        image[1024 + 0x06],
        image[1024 + 0x07],
    ]);
    assert_eq!(s_blocks_count, 37);

    // 验证 inode 位图：inode 1..10 为保留 inode（bit 0..9 = 0x3FF），其余未用 inode 位为 0，
    // 与 superblock/GDT 的 free_inodes = inodes_per_group - 10 = 246 一致。inode bitmap 在 block 3。
    let inode_bitmap_lo = image[3 * 4096]; // inode bitmap 在 block 3
    let inode_bitmap_hi = image[3 * 4096 + 1];
    assert_eq!(
        u16::from_le_bytes([inode_bitmap_lo, inode_bitmap_hi]),
        0x3FF,
        "inode 位图低 16 位应标记 inode 1..10 已用（0x3FF），与空闲计数一致"
    );
    // 空闲 inode 计数（superblock 0x10）= inodes_per_group - 10 = 246
    let s_free_inodes = u32::from_le_bytes([
        image[1024 + 0x10],
        image[1024 + 0x11],
        image[1024 + 0x12],
        image[1024 + 0x13],
    ]);
    assert_eq!(s_free_inodes, INODES_PER_GROUP - 10);

    // 验证根目录 inode（inode table 在 block 4）
    let inode_table_start = 4 * 4096;
    let root_inode_off = inode_table_start + 256;
    let i_mode = u16::from_le_bytes([image[root_inode_off], image[root_inode_off + 1]]);
    assert_eq!(i_mode, 0x41ED);

    // 验证根目录数据块位置
    // inode_table_blocks = (256 * 256 + 4095) / 4096 = 16
    // gdt=1, block bitmap=2, inode bitmap=3, inode table=4..19, root dir=20
    // 验证 inode 指向正确的数据块
    let i_block0 = u32::from_le_bytes([
        image[root_inode_off + 0x28],
        image[root_inode_off + 0x29],
        image[root_inode_off + 0x2A],
        image[root_inode_off + 0x2B],
    ]);
    assert_eq!(i_block0, 20); // root_dir_blk = 20

    // 验证根目录数据块
    let dir_block_start = 20 * 4096;
    let inode = u32::from_le_bytes([
        image[dir_block_start],
        image[dir_block_start + 1],
        image[dir_block_start + 2],
        image[dir_block_start + 3],
    ]);
    assert_eq!(inode, 2); // . entry point to inode 2
    // name_len 在 offset 6 (inode 4 + rec_len 2)
    assert_eq!(image[dir_block_start + 6], 1); // name_len = 1
    // name 在 offset 8 (inode 4 + rec_len 2 + name_len 1 + file_type 1)
    assert_eq!(image[dir_block_start + 8], b'.'); // name = "."

    // mtkclient-rs 的 format 路径直写 raw 镜像（DA 流式写入，不走 sparse 封装）；
    // 结构断言已在上方覆盖。

    // 超大分区验证：生成器内部一致，与分区大小无关（仍只产出 21 块结构前缀）
    let big = create_minimal_ext4(238u64 * 1024 * 1024 * 1024);
    assert_eq!(big.len(), 21 * 4096);
}

#[test]
fn test_parse_u64_hex_variants() {
    // 带 0x 前缀
    assert_eq!(parse_u64_hex("0x1A638F8000").unwrap(), 0x001A_638F_8000);
    // 无前缀的十六进制（MediaTek LK 的 partition-size 变量格式）
    assert_eq!(parse_u64_hex("1a638f8000").unwrap(), 0x001A_638F_8000);
    assert_eq!(parse_u64_hex("80000").unwrap(), 0x80000);
    // 带空格
    assert_eq!(parse_u64_hex(" 0x280000000 ").unwrap(), 0x2_8000_0000);
}