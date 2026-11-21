//! Minimal f2fs filesystem image generation
//!
//! Generates a minimal f2fs filesystem image mountable by the Linux kernel,
//! containing a valid superblock, checkpoint, NAT, SIT, SSA, root inode, and root directory.
//! Supports a 4096-byte block size and a 2MB segment size.

#![allow(clippy::too_many_arguments)]
mod layout;
use layout::*;

/// f2fs magic number
const F2FS_MAGIC: u32 = 0xF2F52010;

/// Block size
const BLOCK_SIZE: usize = 4096;

/// Number of blocks per segment
const BLOCKS_PER_SEG: u32 = 512;

/// Log2 of blocks per segment
const LOG_BLOCKS_PER_SEG: u32 = 9;

/// Reserved nids: 0, 1, 2 are system-reserved, 3 is the root directory
const ROOT_NID: u32 = 3;

/// Entries per NAT block (4096 / 9 = 455)
const NAT_ENTRY_PER_BLOCK: u32 = 455;

/// Entries per SIT block (4096 / 74 = 55)
const SIT_ENTRY_PER_BLOCK: u32 = 55;

/// Default volume label
const VOLUME_LABEL: &[u8] = b"fastboot-rs";

/// Directory file type: 2 = directory
const FILE_TYPE_DIR: u8 = 2;

/// Offset of the root directory data block (block number offset within the main area)
const ROOT_DATA_BLOCK_OFFSET: u32 = 1;

// ==================== Offset constants ====================

// Superblock offsets (relative to the superblock start, i.e. in-block offset 1024)
const SB_OFFSET: usize = 1024;

// The following offsets are relative to SB_OFFSET
const SB_MAGIC: usize = 0x000;
const SB_MAJOR_VER: usize = 0x004;
const SB_MINOR_VER: usize = 0x006;
const SB_LOG_SECTORSIZE: usize = 0x008;
const SB_LOG_SECTORS_PER_BLOCK: usize = 0x00C;
const SB_LOG_BLOCKSIZE: usize = 0x010;
const SB_LOG_BLOCKS_PER_SEG: usize = 0x014;
const SB_SEGS_PER_SEC: usize = 0x018;
const SB_SECS_PER_ZONE: usize = 0x01C;
const SB_CHECKSUM_OFFSET: usize = 0x020;
const SB_BLOCK_COUNT: usize = 0x024;
const SB_SECTION_COUNT: usize = 0x02C;
const SB_SEGMENT_COUNT: usize = 0x030;
const SB_SEGMENT_COUNT_CKPT: usize = 0x034;
const SB_SEGMENT_COUNT_SIT: usize = 0x038;
const SB_SEGMENT_COUNT_NAT: usize = 0x03C;
const SB_SEGMENT_COUNT_SSA: usize = 0x040;
const SB_SEGMENT_COUNT_MAIN: usize = 0x044;
const SB_SEGMENT0_BLKADDR: usize = 0x048;
const SB_CP_BLKADDR: usize = 0x04C;
const SB_SIT_BLKADDR: usize = 0x050;
const SB_NAT_BLKADDR: usize = 0x054;
const SB_SSA_BLKADDR: usize = 0x058;
const SB_MAIN_BLKADDR: usize = 0x05C;
const SB_ROOT_INO: usize = 0x060;
const SB_NODE_INO: usize = 0x064;
const SB_META_INO: usize = 0x068;
const SB_UUID: usize = 0x06C;
const SB_VOLUME_NAME: usize = 0x07C;
const SB_EXTENSION_COUNT: usize = 0x47C;
const SB_CP_PAYLOAD: usize = 0x680;
const SB_VERSION: usize = 0x684;
const SB_INIT_VERSION: usize = 0x784;
const SB_FEATURE: usize = 0x884;
const SB_CRC: usize = 0xBFC;

// Checkpoint offsets
const CP_CHECKPOINT_VER: usize = 0x000;
const CP_USER_BLOCK_COUNT: usize = 0x008;
const CP_VALID_BLOCK_COUNT: usize = 0x010;
const CP_RSVD_SEGMENT_COUNT: usize = 0x018;
const CP_OVERPROV_SEGMENT_COUNT: usize = 0x01C;
const CP_FREE_SEGMENT_COUNT: usize = 0x020;
const CP_CUR_NODE_SEGNO: usize = 0x024; // 8 * 4 = 32 bytes
const CP_CUR_NODE_BLKOFF: usize = 0x044; // 8 * 2 = 16 bytes
const CP_CUR_DATA_SEGNO: usize = 0x054; // 8 * 4 = 32 bytes
const CP_CUR_DATA_BLKOFF: usize = 0x074; // 8 * 2 = 16 bytes
const CP_CKPT_FLAGS: usize = 0x084;
const CP_CP_PACK_TOTAL_BLOCK_COUNT: usize = 0x088;
const CP_CP_PACK_START_SUM: usize = 0x08C;
const CP_VALID_NODE_COUNT: usize = 0x090;
const CP_VALID_INODE_COUNT: usize = 0x094;
const CP_NEXT_FREE_NID: usize = 0x098;
const CP_SIT_VER_BITMAP_BYTESIZE: usize = 0x09C;
const CP_NAT_VER_BITMAP_BYTESIZE: usize = 0x0A0;
const CP_CHECKSUM_OFFSET: usize = 0x0A4;
const CP_ELAPSED_TIME: usize = 0x0A8;
const CP_ALLOC_TYPE: usize = 0x0B0; // 16 bytes
const CP_SIT_NAT_BITMAP: usize = 0x0C0;

// Root inode offsets (relative to the node block start)
const INODE_I_MODE: usize = 0x000;
const INODE_I_ADVISE: usize = 0x002;
const INODE_I_INLINE: usize = 0x003;
const INODE_I_UID: usize = 0x004;
const INODE_I_GID: usize = 0x008;
const INODE_I_LINKS: usize = 0x00C;
const INODE_I_SIZE: usize = 0x010;
const INODE_I_BLOCKS: usize = 0x018;
const INODE_I_ADDR: usize = 0x168; // 0x168 = 360, i_addr[0] start
// Node footer offsets (relative to the node block start, starting at 4072)
const NODE_FOOTER_NID: usize = 0xFE8;
const NODE_FOOTER_INO: usize = 0xFEC;
const NODE_FOOTER_FLAG: usize = 0xFF0;
const NODE_FOOTER_CP_VER: usize = 0xFF4;
const NODE_FOOTER_NEXT_BLKADDR: usize = 0xFFC;

// Dentry block offsets
const DENTRY_BITMAP: usize = 0x000; // 27 bytes
const DENTRY_ENTRIES: usize = 0x01E; // 2354 bytes (214 * 11)
const DENTRY_FILENAME: usize = 0x95C; // 1712 bytes (214 * 8)

// ==================== Main function ====================

/// Generate a minimal f2fs filesystem image
///
/// ## Design constraint (formatting large partitions, e.g. userdata of hundreds of GB)
///
/// When the caller writes a raw image directly, no sparse wrapping is needed (mtkclient-rs's DA write path
/// is raw streaming). (Only structure blocks are flashed, not the whole partition) — the remaining area is
/// zeroed by `erase` before format and lies outside the filesystem. Therefore this generator's output
/// filesystem **declares only the number of blocks/segments occupied by its own structures** (~9 segments
/// ≈ 18MB), not the whole partition size. This differs from official `make_f2fs` filling the partition, but
/// it is the only internally consistent approach under our "erase + write structures only" scheme
/// (otherwise `total_blocks` would far exceed the structures actually written, and the kernel would refuse to mount).
///
/// A viable minimal f2fs layout needs at least 9 segments: SB(1) + CP(2) + SIT(≥1) + NAT(≥1) + SSA(≥1) + MAIN(≥1).
/// This implementation fixes seg_ckpt/seg_sit/seg_nat = 2 each (`main_blkaddr` is always 4096), the structure
/// area extends at most to `main_blkaddr + 2`, so `total_blocks` is always 4098 → `total_segments = 9`,
/// `seg_main = 1` (just enough for the root inode + root directory data block). This layout is consistent for
/// any partition size.
///
/// `partition_size`: partition size (bytes), used only for the minimum validity check.
pub fn create_minimal_f2fs(partition_size: u64) -> Vec<u8> {
    // A viable minimal f2fs structure occupies 9 segments (≈18MB); the partition must at least hold the structure area.
    assert!(
        partition_size >= 9 * BLOCKS_PER_SEG as u64 * BLOCK_SIZE as u64,
        "分区太小，无法容纳 f2fs 结构区（至少需 {} 字节）",
        9 * BLOCKS_PER_SEG as u64 * BLOCK_SIZE as u64
    );

    // Block addresses (fixed layout, independent of partition size)
    let seg0_blkaddr: u32 = 0;
    let cp_blkaddr: u32 = BLOCKS_PER_SEG; // 512
    let seg_ckpt: u32 = 2;
    let sit_blkaddr: u32 = cp_blkaddr + seg_ckpt * BLOCKS_PER_SEG; // 1536
    let seg_sit: u32 = 2;
    let nat_blkaddr: u32 = sit_blkaddr + seg_sit * BLOCKS_PER_SEG; // 2560
    let seg_nat: u32 = 2;
    let ssa_blkaddr: u32 = nat_blkaddr + seg_nat * BLOCKS_PER_SEG; // 3584
    let seg_ssa: u32 = 1;
    let main_blkaddr: u32 = ssa_blkaddr + seg_ssa * BLOCKS_PER_SEG; // 4096

    // The filesystem declares only the blocks/segments occupied by its own structures (a single main segment holds the root inode + root directory).
    let total_blocks = main_blkaddr + ROOT_DATA_BLOCK_OFFSET + 1; // 4098
    let total_segments = total_blocks.div_ceil(BLOCKS_PER_SEG); // 9
    let seg_main = total_segments - 1 - seg_ckpt - seg_sit - seg_nat - seg_ssa; // 1

    let root_ino = ROOT_NID;
    let node_ino: u32 = 1;
    let meta_ino: u32 = 2;

    // Byte sizes of the SIT / NAT version bitmaps (inside the checkpoint): computed as "1 bit per metadata block",
    // replacing the previously hardcoded 64 / 256, so they stay consistent with the seg_sit / seg_nat segment counts
    // (covering all main-area segments).
    let sit_ver_bitmap_bytesize = (seg_sit * BLOCKS_PER_SEG).div_ceil(8);
    let nat_ver_bitmap_bytesize = (seg_nat * BLOCKS_PER_SEG).div_ceil(8);

    // Allocate memory only for the "structure area" (superblock/checkpoint/SIT/NAT/SSA + main-area root inode/root
    // dir data block, at most up to main_blkaddr + ROOT_DATA_BLOCK_OFFSET), avoiding allocation for the whole
    // partition — formatting a large partition (e.g. userdata of hundreds of GB) with `vec![0u8; partition_size]`
    // would OOM. The remaining area is all zero, represented by sparse DONT_CARE on the caller side.
    let structure_len =
        ((main_blkaddr + ROOT_DATA_BLOCK_OFFSET + 1) as u64 * BLOCK_SIZE as u64) as usize;
    let mut image = vec![0u8; structure_len];

    // ==================== Superblock (blocks 0 and 1) ====================
    write_superblock(
        &mut image,
        SB_OFFSET,
        total_blocks,
        total_segments,
        seg_ckpt,
        seg_sit,
        seg_nat,
        seg_ssa,
        seg_main,
        seg0_blkaddr,
        cp_blkaddr,
        sit_blkaddr,
        nat_blkaddr,
        ssa_blkaddr,
        main_blkaddr,
        root_ino,
        node_ino,
        meta_ino,
    );
    // Copy at block 1
    write_superblock(
        &mut image,
        BLOCK_SIZE + SB_OFFSET,
        total_blocks,
        total_segments,
        seg_ckpt,
        seg_sit,
        seg_nat,
        seg_ssa,
        seg_main,
        seg0_blkaddr,
        cp_blkaddr,
        sit_blkaddr,
        nat_blkaddr,
        ssa_blkaddr,
        main_blkaddr,
        root_ino,
        node_ino,
        meta_ino,
    );

    // ==================== Checkpoint (blocks cp_blkaddr and cp_blkaddr + BLOCKS_PER_SEG) ====================
    write_checkpoint(
        &mut image,
        cp_blkaddr as usize * BLOCK_SIZE,
        total_blocks,
        seg_main,
        sit_ver_bitmap_bytesize,
        nat_ver_bitmap_bytesize,
        main_blkaddr,
        nat_blkaddr,
        sit_blkaddr,
        ssa_blkaddr,
    );
    // CP copy
    write_checkpoint(
        &mut image,
        (cp_blkaddr + BLOCKS_PER_SEG) as usize * BLOCK_SIZE,
        total_blocks,
        seg_main,
        sit_ver_bitmap_bytesize,
        nat_ver_bitmap_bytesize,
        main_blkaddr,
        nat_blkaddr,
        sit_blkaddr,
        ssa_blkaddr,
    );

    // ==================== SIT (block sit_blkaddr) ====================
    // Mark all used segments: the first 8 are metadata segments (SB/CP/SIT/NAT/SSA), the 9th (seg 8) is the
    // root main-area segment, containing the root inode (main_blkaddr) + root dir data block (main_blkaddr+1), vblocks=2.
    let sit_block_offset = sit_blkaddr as usize * BLOCK_SIZE;
    for seg_idx in 0..total_segments as usize {
        // Segment 0 (SB): blocks 0,1 used
        // Segment 1 (CP1): block 512 used
        // Segment 2 (CP2): block 1024 used
        // Segment 3 (SIT1): block 1536 used
        // Segment 4 (SIT2): 0 used
        // Segment 5 (NAT1): block 2560 used
        // Segment 6 (NAT2): 0 used
        // Segment 7 (SSA): block 3584 used
        // Segment 8 (root main): root inode + root dir data = 2 blocks
        let vblocks = match seg_idx {
            0 => 2,     // SB: 2 blocks
            1..=7 => 1, // CP, SIT, NAT, SSA: 1 block each
            8 => 2,     // root main: root inode + root dir data
            _ => 0,
        };
        let block_idx = sit_block_offset + seg_idx / SIT_ENTRY_PER_BLOCK as usize * BLOCK_SIZE;
        let entry_idx = seg_idx % SIT_ENTRY_PER_BLOCK as usize;
        write_sit_entry(&mut image, block_idx, entry_idx, vblocks);
    }

    // ==================== NAT (block nat_blkaddr) ====================
    // Root inode NAT entry: nid=3, block_addr=main_blkaddr
    let nat_block_offset = nat_blkaddr as usize * BLOCK_SIZE;
    let nat_entry_idx = ROOT_NID as usize % NAT_ENTRY_PER_BLOCK as usize;
    let nat_entry_offset = nat_entry_idx * 9;
    write_u32_le(
        &mut image,
        nat_block_offset + nat_entry_offset + 1,
        root_ino,
    ); // ino
    write_u32_le(
        &mut image,
        nat_block_offset + nat_entry_offset + 5,
        main_blkaddr,
    ); // block_addr

    // ==================== SSA (block ssa_blkaddr) ====================
    // Only the SSA entry for the block containing the root inode is needed
    // Each summary entry is 7 bytes, corresponding to each block in the main area
    // The root inode is at main_blkaddr (main-area offset 0)
    // Each segment in SSA has one summary block (512 entries)
    // Main-area block 0 (main_blkaddr) maps to segment 0 entry 0 in SSA
    let ssa_block_offset = ssa_blkaddr as usize * BLOCK_SIZE;
    // summary entry 0: nid = root_ino, ofs_in_node = 0
    write_u32_le(&mut image, ssa_block_offset, root_ino); // nid
    // ofs_in_node = 0 (already zero)
    // version = 0 (already zero)
    // footer: entry_type = 1 (NODE)
    write_u8(&mut image, ssa_block_offset + BLOCK_SIZE - 5, 1); // SUM_TYPE_NODE

    // SSA entry for the root directory data block (main_blkaddr + 1)
    // Main-area block 1 (main_blkaddr + 1) maps to segment 0 entry 1 in SSA
    write_u32_le(&mut image, ssa_block_offset + 7, root_ino); // nid
    write_u8(&mut image, ssa_block_offset + 7 + 4, 1); // ofs_in_node = 1 (data block index)

    // ==================== Root inode (block main_blkaddr) ====================
    let root_node_offset = main_blkaddr as usize * BLOCK_SIZE;
    // i_mode: S_IFDIR | 0755 = 0x41ED
    write_u16_le(&mut image, root_node_offset + INODE_I_MODE, 0x41ED);
    // i_advise = 0
    write_u8(&mut image, root_node_offset + INODE_I_ADVISE, 0);
    // i_inline = 0
    write_u8(&mut image, root_node_offset + INODE_I_INLINE, 0);
    // i_uid = 0
    write_u32_le(&mut image, root_node_offset + INODE_I_UID, 0);
    // i_gid = 0
    write_u32_le(&mut image, root_node_offset + INODE_I_GID, 0);
    // i_links = 2 (for "." and "..")
    write_u32_le(&mut image, root_node_offset + INODE_I_LINKS, 2);
    // i_size = 4096 (1 block for directory entries)
    write_u64_le(
        &mut image,
        root_node_offset + INODE_I_SIZE,
        BLOCK_SIZE as u64,
    );
    // i_blocks = 2 blocks = 16 sectors (512-byte sectors in f2fs)
    write_u64_le(&mut image, root_node_offset + INODE_I_BLOCKS, 16);

    // i_addr`0` = physical block of root directory data
    write_u32_le(
        &mut image,
        root_node_offset + INODE_I_ADDR,
        main_blkaddr + ROOT_DATA_BLOCK_OFFSET,
    );

    // Node footer
    write_u32_le(&mut image, root_node_offset + NODE_FOOTER_NID, root_ino);
    write_u32_le(&mut image, root_node_offset + NODE_FOOTER_INO, root_ino);
    write_u32_le(&mut image, root_node_offset + NODE_FOOTER_FLAG, 0);
    write_u64_le(&mut image, root_node_offset + NODE_FOOTER_CP_VER, 0);
    write_u32_le(&mut image, root_node_offset + NODE_FOOTER_NEXT_BLKADDR, 0);

    // ==================== Root directory data block (block main_blkaddr + 1) ====================
    let root_data_offset = (main_blkaddr + ROOT_DATA_BLOCK_OFFSET) as usize * BLOCK_SIZE;

    // Set the dentry bitmap: bit 0 and bit 1 set (entries "." and "..")
    image[root_data_offset + DENTRY_BITMAP] = 0x03;

    // Entry 0: "." -> inode = root_ino, name_len = 1, file_type = 2
    let entry0_offset = root_data_offset + DENTRY_ENTRIES;
    write_u32_le(&mut image, entry0_offset, hash_filename(b"."));
    write_u32_le(&mut image, entry0_offset + 4, root_ino);
    write_u16_le(&mut image, entry0_offset + 8, 1); // name_len
    write_u8(&mut image, entry0_offset + 10, FILE_TYPE_DIR);
    // filename in slot 0
    image[root_data_offset + DENTRY_FILENAME] = b'.';

    // Entry 1: ".." -> inode = root_ino, name_len = 2, file_type = 2
    let entry1_offset = root_data_offset + DENTRY_ENTRIES + 11;
    write_u32_le(&mut image, entry1_offset, hash_filename(b".."));
    write_u32_le(&mut image, entry1_offset + 4, root_ino);
    write_u16_le(&mut image, entry1_offset + 8, 2); // name_len
    write_u8(&mut image, entry1_offset + 10, FILE_TYPE_DIR);
    // filename in slot 1 (8 bytes)
    image[root_data_offset + DENTRY_FILENAME + 8] = b'.';
    image[root_data_offset + DENTRY_FILENAME + 9] = b'.';

    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_minimal_f2fs_128mb() {
        let partition_size = 128 * 1024 * 1024u64;
        let image = create_minimal_f2fs(partition_size);
        // Structure prefix (superblock/CP/SIT/NAT/SSA + main-area root inode/dir data block),
        // length = (main_blkaddr=4096 + ROOT_DATA_BLOCK_OFFSET=1 + 1) * 4096, independent of partition size
        assert_eq!(image.len(), 4098 * 4096);

        // Verify the superblock magic
        let magic = u32::from_le_bytes([
            image[SB_OFFSET + SB_MAGIC],
            image[SB_OFFSET + SB_MAGIC + 1],
            image[SB_OFFSET + SB_MAGIC + 2],
            image[SB_OFFSET + SB_MAGIC + 3],
        ]);
        assert_eq!(magic, F2FS_MAGIC);

        // Verify the block size
        let log_blocksize = u32::from_le_bytes([
            image[SB_OFFSET + SB_LOG_BLOCKSIZE],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 1],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 2],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 3],
        ]);
        assert_eq!(log_blocksize, 12);

        // Verify the root inode number
        let root_ino = u32::from_le_bytes([
            image[SB_OFFSET + SB_ROOT_INO],
            image[SB_OFFSET + SB_ROOT_INO + 1],
            image[SB_OFFSET + SB_ROOT_INO + 2],
            image[SB_OFFSET + SB_ROOT_INO + 3],
        ]);
        assert_eq!(root_ino, 3);

        // Verify the main-area start block address
        let main_blkaddr = u32::from_le_bytes([
            image[SB_OFFSET + SB_MAIN_BLKADDR],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 1],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 2],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 3],
        ]);
        assert_eq!(main_blkaddr, 4096);

        // Verify the root inode's i_mode
        let root_node_offset = main_blkaddr as usize * BLOCK_SIZE;
        let i_mode = u16::from_le_bytes([
            image[root_node_offset + INODE_I_MODE],
            image[root_node_offset + INODE_I_MODE + 1],
        ]);
        assert_eq!(i_mode, 0x41ED);

        // Verify the root directory data block
        let root_data_offset = (main_blkaddr + ROOT_DATA_BLOCK_OFFSET) as usize * BLOCK_SIZE;
        // Check the dentry bitmap: bit 0 and bit 1 should be 1
        assert_eq!(image[root_data_offset + DENTRY_BITMAP] & 0x03, 0x03);

        // Check entry 0: "." inode = root_ino
        let entry0_offset = root_data_offset + DENTRY_ENTRIES;
        let entry0_ino = u32::from_le_bytes([
            image[entry0_offset + 4],
            image[entry0_offset + 5],
            image[entry0_offset + 6],
            image[entry0_offset + 7],
        ]);
        assert_eq!(entry0_ino, 3);
        assert_eq!(image[root_data_offset + DENTRY_FILENAME], b'.');

        // Check entry 1: ".." inode = root_ino
        let entry1_offset = root_data_offset + DENTRY_ENTRIES + 11;
        let entry1_ino = u32::from_le_bytes([
            image[entry1_offset + 4],
            image[entry1_offset + 5],
            image[entry1_offset + 6],
            image[entry1_offset + 7],
        ]);
        assert_eq!(entry1_ino, 3);
        assert_eq!(image[root_data_offset + DENTRY_FILENAME + 8], b'.');
        assert_eq!(image[root_data_offset + DENTRY_FILENAME + 9], b'.');

        // mtkclient-rs's format path writes the raw image directly (without sparse wrapping),
        // so the structure prefix is the complete image; the length assertion above covers it.
    }

    #[test]
    fn test_create_minimal_f2fs_238gb() {
        // Large partition (e.g. userdata 238GB): the structure prefix length is still always 4098*4096, independent of partition size
        let partition_size = 238u64 * 1024 * 1024 * 1024;
        let image = create_minimal_f2fs(partition_size);
        assert_eq!(image.len(), 4098 * 4096);

        // For a large partition the structure prefix is constant: independent of partition size (sparse-wrapping assertions live in the fastboot-rs tests).
    }

    #[test]
    fn test_hash_filename() {
        let h = hash_filename(b".");
        // 0 * 13 + 46 = 46
        assert_eq!(h, 46);
        let h2 = hash_filename(b"..");
        // 46 * 13 + 46 = 644
        assert_eq!(h2, 644);
    }
}
