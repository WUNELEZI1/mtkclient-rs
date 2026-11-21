//! Minimal f2fs filesystem image generation
//!
//! Generates a minimal f2fs image that the Linux kernel can mount,
//! containing a valid superblock, checkpoint, NAT, SIT, SSA, root inode and root directory.
//! Supports a 4096-byte block size and a 2MB segment size.

#![allow(clippy::too_many_arguments)]
use super::*;

// ==================== Superblock writing ====================

pub(crate) fn write_superblock(
    image: &mut [u8],
    offset: usize,
    total_blocks: u32,
    total_segments: u32,
    seg_ckpt: u32,
    seg_sit: u32,
    seg_nat: u32,
    seg_ssa: u32,
    seg_main: u32,
    seg0_blkaddr: u32,
    cp_blkaddr: u32,
    sit_blkaddr: u32,
    nat_blkaddr: u32,
    ssa_blkaddr: u32,
    main_blkaddr: u32,
    root_ino: u32,
    node_ino: u32,
    meta_ino: u32,
) {
    let sb = offset;

    write_u32_le(image, sb + SB_MAGIC, F2FS_MAGIC);
    write_u16_le(image, sb + SB_MAJOR_VER, 1);
    write_u16_le(image, sb + SB_MINOR_VER, 0);
    write_u32_le(image, sb + SB_LOG_SECTORSIZE, 9); // 512 bytes
    write_u32_le(image, sb + SB_LOG_SECTORS_PER_BLOCK, 3); // 8 sectors per block
    write_u32_le(image, sb + SB_LOG_BLOCKSIZE, 12); // 4096 bytes
    write_u32_le(image, sb + SB_LOG_BLOCKS_PER_SEG, LOG_BLOCKS_PER_SEG);
    write_u32_le(image, sb + SB_SEGS_PER_SEC, 1);
    write_u32_le(image, sb + SB_SECS_PER_ZONE, 1);
    write_u32_le(image, sb + SB_CHECKSUM_OFFSET, 0);
    write_u64_le(image, sb + SB_BLOCK_COUNT, total_blocks as u64);
    write_u32_le(image, sb + SB_SECTION_COUNT, total_segments);
    write_u32_le(image, sb + SB_SEGMENT_COUNT, total_segments);
    write_u32_le(image, sb + SB_SEGMENT_COUNT_CKPT, seg_ckpt);
    write_u32_le(image, sb + SB_SEGMENT_COUNT_SIT, seg_sit);
    write_u32_le(image, sb + SB_SEGMENT_COUNT_NAT, seg_nat);
    write_u32_le(image, sb + SB_SEGMENT_COUNT_SSA, seg_ssa);
    write_u32_le(image, sb + SB_SEGMENT_COUNT_MAIN, seg_main);
    write_u32_le(image, sb + SB_SEGMENT0_BLKADDR, seg0_blkaddr);
    write_u32_le(image, sb + SB_CP_BLKADDR, cp_blkaddr);
    write_u32_le(image, sb + SB_SIT_BLKADDR, sit_blkaddr);
    write_u32_le(image, sb + SB_NAT_BLKADDR, nat_blkaddr);
    write_u32_le(image, sb + SB_SSA_BLKADDR, ssa_blkaddr);
    write_u32_le(image, sb + SB_MAIN_BLKADDR, main_blkaddr);
    write_u32_le(image, sb + SB_ROOT_INO, root_ino);
    write_u32_le(image, sb + SB_NODE_INO, node_ino);
    write_u32_le(image, sb + SB_META_INO, meta_ino);

    // UUID
    let uuid: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    image[sb + SB_UUID..sb + SB_UUID + 16].copy_from_slice(&uuid);

    // Volume label
    for (i, &b) in VOLUME_LABEL.iter().take(16).enumerate() {
        write_u16_le(image, sb + SB_VOLUME_NAME + i * 2, b as u16);
    }

    // Extensions
    write_u32_le(image, sb + SB_EXTENSION_COUNT, 0);

    // CP payload
    write_u32_le(image, sb + SB_CP_PAYLOAD, 0);

    // Version string
    let ver = b"fastboot-rs v0.1.0\0";
    let ver_len = ver.len().min(256);
    image[sb + SB_VERSION..sb + SB_VERSION + ver_len].copy_from_slice(ver);
    image[sb + SB_INIT_VERSION..sb + SB_INIT_VERSION + ver_len].copy_from_slice(ver);

    // Feature flags
    write_u32_le(image, sb + SB_FEATURE, 0);

    // Checksum
    write_u32_le(image, sb + SB_CRC, 0);
}

// ==================== Checkpoint writing ====================

pub(crate) fn write_checkpoint(
    image: &mut [u8],
    offset: usize,
    _total_blocks: u32,
    main_segments: u32,
    sit_ver_bitmap_bytesize: u32,
    nat_ver_bitmap_bytesize: u32,
    _main_blkaddr: u32,
    _nat_blkaddr: u32,
    _sit_blkaddr: u32,
    _ssa_blkaddr: u32,
) {
    let cp = offset;

    // checkpoint_ver = 1
    write_u64_le(image, cp + CP_CHECKPOINT_VER, 1);
    // user_block_count = main area blocks
    write_u64_le(
        image,
        cp + CP_USER_BLOCK_COUNT,
        main_segments as u64 * BLOCKS_PER_SEG as u64,
    );
    // valid_block_count = 2 (root inode + root data)
    write_u64_le(image, cp + CP_VALID_BLOCK_COUNT, 2);
    // rsvd_segment_count = 0
    write_u32_le(image, cp + CP_RSVD_SEGMENT_COUNT, 0);
    // overprov_segment_count = 0
    write_u32_le(image, cp + CP_OVERPROV_SEGMENT_COUNT, 0);
    // free_segment_count = main_segments - 1 (1 segment used for root)
    write_u32_le(image, cp + CP_FREE_SEGMENT_COUNT, main_segments - 1);
    // cur_node_segno`0` = 0 (main segment 0)
    write_u32_le(image, cp + CP_CUR_NODE_SEGNO, 0);
    // cur_node_blkoff`0` = 1 (next write at block 1)
    write_u16_le(image, cp + CP_CUR_NODE_BLKOFF, 1);
    // cur_data_segno`0` = 0
    write_u32_le(image, cp + CP_CUR_DATA_SEGNO, 0);
    // cur_data_blkoff`0` = 2 (next write at block 2, after root data)
    write_u16_le(image, cp + CP_CUR_DATA_BLKOFF, 2);
    // ckpt_flags = CP_UMOUNT_FLAG (clean unmount)
    write_u32_le(image, cp + CP_CKPT_FLAGS, 0x00000001);
    // cp_pack_total_block_count = 1 (just the checkpoint block)
    write_u32_le(image, cp + CP_CP_PACK_TOTAL_BLOCK_COUNT, 1);
    // cp_pack_start_sum = 0 (no summary in checkpoint)
    write_u32_le(image, cp + CP_CP_PACK_START_SUM, 0);
    // valid_node_count = 1 (root inode)
    write_u32_le(image, cp + CP_VALID_NODE_COUNT, 1);
    // valid_inode_count = 1 (root inode)
    write_u32_le(image, cp + CP_VALID_INODE_COUNT, 1);
    // next_free_nid = 4 (start from 4, since 0-3 are reserved/used)
    write_u32_le(image, cp + CP_NEXT_FREE_NID, 4);

    // SIT bitmap size
    write_u32_le(
        image,
        cp + CP_SIT_VER_BITMAP_BYTESIZE,
        sit_ver_bitmap_bytesize,
    );
    // NAT bitmap size
    write_u32_le(
        image,
        cp + CP_NAT_VER_BITMAP_BYTESIZE,
        nat_ver_bitmap_bytesize,
    );
    // checksum_offset = F2FS_BLKSIZE - 4 = 4092
    write_u32_le(image, cp + CP_CHECKSUM_OFFSET, BLOCK_SIZE as u32 - 4);

    // elapsed_time = 0
    write_u64_le(image, cp + CP_ELAPSED_TIME, 0);

    // alloc_type: every log type is CUR_LOG_ALIVE (0)
    for i in 0..16 {
        write_u8(image, cp + CP_ALLOC_TYPE + i, 0);
    }

    // SIT bitmap: mark the first 2 SIT blocks as used
    let sit_bitmap_offset = cp + CP_SIT_NAT_BITMAP;
    // 2 SIT segments = 2 blocks, 2 bits
    // Actually needed: ceil(1+2+2+2+1+1 / 55) = ceil(9/55) = 1 block
    // Set bit 0 to 1 (the first SIT block is in use)
    for i in 0..2 {
        image[sit_bitmap_offset + i] = 0xFF;
    }

    // NAT bitmap: mark the first NAT block as used
    let nat_bitmap_offset = sit_bitmap_offset + sit_ver_bitmap_bytesize as usize;
    image[nat_bitmap_offset] = 0x01;

    // Checksum (last 4 bytes of the block)
    // For the minimal implementation, use a fixed checksum of 0
    write_u32_le(image, cp + BLOCK_SIZE - 4, 0);
}

// ==================== SIT entry writing ====================

pub(crate) fn write_sit_entry(
    image: &mut [u8],
    block_offset: usize,
    entry_idx: usize,
    vblocks: u16,
) {
    let entry_offset = block_offset + entry_idx * 74;
    // vblocks: low 10 bits hold the valid block count, high 6 bits the allocation type
    let vblocks_val = vblocks; // allocation type 0 (CUR_LOG_ALIVE)
    write_u16_le(image, entry_offset, vblocks_val);
    // valid_map: mark the first vblocks blocks as valid
    for i in 0..vblocks as usize {
        let byte_idx = entry_offset + 2 + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }
    // mtime = 0
    write_u64_le(image, entry_offset + 66, 0);
}

// ==================== f2fs directory hash function ====================

/// Compute the f2fs filename hash
pub(crate) fn hash_filename(name: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &c in name {
        hash = hash.wrapping_mul(13).wrapping_add(c as u32);
    }
    hash
}

// ==================== Little-endian write helpers ====================

pub(crate) fn write_u8(buf: &mut [u8], offset: usize, val: u8) {
    buf[offset] = val;
}

pub(crate) fn write_u16_le(buf: &mut [u8], offset: usize, val: u16) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 2].copy_from_slice(&bytes);
}

pub(crate) fn write_u32_le(buf: &mut [u8], offset: usize, val: u32) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 4].copy_from_slice(&bytes);
}

pub(crate) fn write_u64_le(buf: &mut [u8], offset: usize, val: u64) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 8].copy_from_slice(&bytes);
}
