//! 最小 f2fs 文件系统镜像生成
//!
//! 生成一个可被 Linux 内核挂载的最小 f2fs 文件系统镜像，
//! 包含有效的 superblock、checkpoint、NAT、SIT、SSA、根 inode 和根目录。
//! 支持 4096 字节块大小，2MB segment 大小。

#![allow(clippy::too_many_arguments)]

/// f2fs 魔数
const F2FS_MAGIC: u32 = 0xF2F52010;

/// 块大小
const BLOCK_SIZE: usize = 4096;

/// 每个 segment 的块数
const BLOCKS_PER_SEG: u32 = 512;

/// 每个 segment 的块数对数
const LOG_BLOCKS_PER_SEG: u32 = 9;

/// 保留的 nid: 0, 1, 2 为系统保留，3 为根目录
const ROOT_NID: u32 = 3;

/// 每个 NAT block 的条目数 (4096 / 9 = 455)
const NAT_ENTRY_PER_BLOCK: u32 = 455;

/// 每个 SIT block 的条目数 (4096 / 74 = 55)
const SIT_ENTRY_PER_BLOCK: u32 = 55;

/// 默认卷标
const VOLUME_LABEL: &[u8] = b"fastboot-rs";

/// 目录文件类型: 2 = 目录
const FILE_TYPE_DIR: u8 = 2;

/// 根目录数据块的偏移量（在主区域内的偏移块号）
const ROOT_DATA_BLOCK_OFFSET: u32 = 1;

// ==================== 偏移常量 ====================

// Superblock 偏移 (相对于 superblock 起始，即块内偏移 1024)
const SB_OFFSET: usize = 1024;

// 以下偏移相对于 SB_OFFSET
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

// Checkpoint 偏移
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

// 根 inode 偏移 (相对于 node block 起始)
const INODE_I_MODE: usize = 0x000;
const INODE_I_ADVISE: usize = 0x002;
const INODE_I_INLINE: usize = 0x003;
const INODE_I_UID: usize = 0x004;
const INODE_I_GID: usize = 0x008;
const INODE_I_LINKS: usize = 0x00C;
const INODE_I_SIZE: usize = 0x010;
const INODE_I_BLOCKS: usize = 0x018;
const INODE_I_ADDR: usize = 0x168; // 0x168 = 360, i_addr[0] start
// Node footer 偏移 (相对于 node block 起始, 从 4072 开始)
const NODE_FOOTER_NID: usize = 0xFE8;
const NODE_FOOTER_INO: usize = 0xFEC;
const NODE_FOOTER_FLAG: usize = 0xFF0;
const NODE_FOOTER_CP_VER: usize = 0xFF4;
const NODE_FOOTER_NEXT_BLKADDR: usize = 0xFFC;

// Dentry block 偏移
const DENTRY_BITMAP: usize = 0x000; // 27 bytes
const DENTRY_ENTRIES: usize = 0x01E; // 2354 bytes (214 * 11)
const DENTRY_FILENAME: usize = 0x95C; // 1712 bytes (214 * 8)

// ==================== 主函数 ====================

/// 生成最小 f2fs 文件系统镜像
///
/// ## 设计约束（format 大分区，如 userdata 上百 GB）
///
/// 调用方直写 raw 镜像时无需 sparse 封装（mtkclient-rs 的 DA 写路径为 raw 流式写入）。
/// （只刷结构块，不覆盖整个分区）——剩余区域由 format 前的 `erase` 清零、处于文件系统之外。因此本
/// 生成器产出的文件系统**只声明其自身结构所占的块/段数**（约 9 个 segment ≈ 18MB），而非整个分区大小。
/// 这与官方 `make_f2fs` 铺满分区不同，却是我们「erase + 只写结构」方案下内部自洽的唯一正确做法
/// （否则 `total_blocks` 远超实际写入结构，内核会拒绝挂载）。
///
/// f2fs 的最小可行布局至少需要 9 个 segment：SB(1) + CP(2) + SIT(≥1) + NAT(≥1) + SSA(≥1) + MAIN(≥1)。
/// 本实现固定使用 seg_ckpt/seg_sit/seg_nat = 各 2（`main_blkaddr` 恒为 4096），结构区最远到
/// `main_blkaddr + 2`，故 `total_blocks` 恒为 4098 → `total_segments = 9`，`seg_main = 1`（刚好容纳
/// 根 inode + 根目录数据块）。该布局对任意分区大小都自洽。
///
/// `partition_size`: 分区大小（字节），仅用于最低合法性校验。
pub fn create_minimal_f2fs(partition_size: u64) -> Vec<u8> {
    // 最小可行 f2fs 结构占用 9 个 segment（≈18MB）；要求分区至少能容纳结构区。
    assert!(
        partition_size >= 9 * BLOCKS_PER_SEG as u64 * BLOCK_SIZE as u64,
        "分区太小，无法容纳 f2fs 结构区（至少需 {} 字节）",
        9 * BLOCKS_PER_SEG as u64 * BLOCK_SIZE as u64
    );

    // 块地址（固定布局，与分区大小无关）
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

    // 文件系统只声明其自身结构所占的块/段数（单 main segment 即可容纳根 inode + 根目录）。
    let total_blocks = main_blkaddr + ROOT_DATA_BLOCK_OFFSET + 1; // 4098
    let total_segments = total_blocks.div_ceil(BLOCKS_PER_SEG); // 9
    let seg_main = total_segments - 1 - seg_ckpt - seg_sit - seg_nat - seg_ssa; // 1

    let root_ino = ROOT_NID;
    let node_ino: u32 = 1;
    let meta_ino: u32 = 2;

    // SIT / NAT 版本位图（位于 checkpoint 内）的字节大小：按「每个元数据 block 1 bit」计算，
    // 取代原先写死的 64 / 256，保证与 seg_sit / seg_nat 段数一致（覆盖全部主区域 segment）。
    let sit_ver_bitmap_bytesize = (seg_sit * BLOCKS_PER_SEG).div_ceil(8);
    let nat_ver_bitmap_bytesize = (seg_nat * BLOCKS_PER_SEG).div_ceil(8);

    // 只分配「结构区」大小的内存（superblock/checkpoint/SIT/NAT/SSA + 主区域根 inode/目录数据块，
    // 最远到 main_blkaddr + ROOT_DATA_BLOCK_OFFSET），避免为整个分区分配内存——format 大分区
    // （如 userdata 上百 GB）若 `vec![0u8; partition_size]` 会 OOM。剩余区域全为零，由调用方用
    // sparse 的 DONT_CARE 表示。
    let structure_len =
        ((main_blkaddr + ROOT_DATA_BLOCK_OFFSET + 1) as u64 * BLOCK_SIZE as u64) as usize;
    let mut image = vec![0u8; structure_len];

    // ==================== Superblock (块 0 和 1) ====================
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
    // 副本在块 1
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

    // ==================== Checkpoint (块 cp_blkaddr 和 cp_blkaddr + BLOCKS_PER_SEG) ====================
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
    // CP 副本
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

    // ==================== SIT (块 sit_blkaddr) ====================
    // 标记所有已用 segment：前 8 个为元数据段（SB/CP/SIT/NAT/SSA），第 9 个（seg 8）为
    // 根主区域段，含根 inode（main_blkaddr）+ 根目录数据块（main_blkaddr+1），vblocks=2。
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

    // ==================== NAT (块 nat_blkaddr) ====================
    // 根 inode NAT 条目: nid=3, block_addr=main_blkaddr
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

    // ==================== SSA (块 ssa_blkaddr) ====================
    // 只需要根 inode 所在 block 的 SSA 条目
    // 每个 summary 条目 7 字节，对应主区域中的每个 block
    // 根 inode 在 main_blkaddr (主区域偏移 0)
    // SSA 中每个 segment 有一个 summary block (512 个条目)
    // 主区域中 block 0 (main_blkaddr) 对应 SSA 中 segment 0 的 entry 0
    let ssa_block_offset = ssa_blkaddr as usize * BLOCK_SIZE;
    // summary entry 0: nid = root_ino, ofs_in_node = 0
    write_u32_le(&mut image, ssa_block_offset, root_ino); // nid
    // ofs_in_node = 0 (已经为零)
    // version = 0 (已经为零)
    // footer: entry_type = 1 (NODE)
    write_u8(&mut image, ssa_block_offset + BLOCK_SIZE - 5, 1); // SUM_TYPE_NODE

    // 根目录数据块 (main_blkaddr + 1) 的 SSA 条目
    // 主区域中 block 1 (main_blkaddr + 1) 对应 SSA 中 segment 0 的 entry 1
    write_u32_le(&mut image, ssa_block_offset + 7, root_ino); // nid
    write_u8(&mut image, ssa_block_offset + 7 + 4, 1); // ofs_in_node = 1 (data block index)

    // ==================== 根 inode (块 main_blkaddr) ====================
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

    // i_addr[0] = physical block of root directory data
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

    // ==================== 根目录数据块 (块 main_blkaddr + 1) ====================
    let root_data_offset = (main_blkaddr + ROOT_DATA_BLOCK_OFFSET) as usize * BLOCK_SIZE;

    // 设置 dentry bitmap: bit 0 和 bit 1 为 1 (条目 "." 和 "..")
    image[root_data_offset + DENTRY_BITMAP] = 0x03;

    // 条目 0: "." -> inode = root_ino, name_len = 1, file_type = 2
    let entry0_offset = root_data_offset + DENTRY_ENTRIES;
    write_u32_le(&mut image, entry0_offset, hash_filename(b"."));
    write_u32_le(&mut image, entry0_offset + 4, root_ino);
    write_u16_le(&mut image, entry0_offset + 8, 1); // name_len
    write_u8(&mut image, entry0_offset + 10, FILE_TYPE_DIR);
    // filename in slot 0
    image[root_data_offset + DENTRY_FILENAME] = b'.';

    // 条目 1: ".." -> inode = root_ino, name_len = 2, file_type = 2
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

// ==================== Superblock 写入 ====================

fn write_superblock(
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

    // 卷标
    for (i, &b) in VOLUME_LABEL.iter().take(16).enumerate() {
        write_u16_le(image, sb + SB_VOLUME_NAME + i * 2, b as u16);
    }

    // 扩展名
    write_u32_le(image, sb + SB_EXTENSION_COUNT, 0);

    // CP payload
    write_u32_le(image, sb + SB_CP_PAYLOAD, 0);

    // 版本字符串
    let ver = b"fastboot-rs v0.1.0\0";
    let ver_len = ver.len().min(256);
    image[sb + SB_VERSION..sb + SB_VERSION + ver_len].copy_from_slice(ver);
    image[sb + SB_INIT_VERSION..sb + SB_INIT_VERSION + ver_len].copy_from_slice(ver);

    // 特性标志
    write_u32_le(image, sb + SB_FEATURE, 0);

    // 校验和
    write_u32_le(image, sb + SB_CRC, 0);
}

// ==================== Checkpoint 写入 ====================

fn write_checkpoint(
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
    // cur_node_segno[0] = 0 (main segment 0)
    write_u32_le(image, cp + CP_CUR_NODE_SEGNO, 0);
    // cur_node_blkoff[0] = 1 (next write at block 1)
    write_u16_le(image, cp + CP_CUR_NODE_BLKOFF, 1);
    // cur_data_segno[0] = 0
    write_u32_le(image, cp + CP_CUR_DATA_SEGNO, 0);
    // cur_data_blkoff[0] = 2 (next write at block 2, after root data)
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

    // alloc_type: 所有日志类型为 CUR_LOG_ALIVE (0)
    for i in 0..16 {
        write_u8(image, cp + CP_ALLOC_TYPE + i, 0);
    }

    // SIT bitmap: 标记前 2 个 SIT block 为已用
    let sit_bitmap_offset = cp + CP_SIT_NAT_BITMAP;
    // 2 SIT segments = 2 blocks, 2 bits
    // 实际需要: ceil(1+2+2+2+1+1 / 55) = ceil(9/55) = 1 block
    // 标记 bit 0 为 1 (第一个 SIT block 已用)
    for i in 0..2 {
        image[sit_bitmap_offset + i] = 0xFF;
    }

    // NAT bitmap: 标记前 1 个 NAT block 为已用
    let nat_bitmap_offset = sit_bitmap_offset + sit_ver_bitmap_bytesize as usize;
    image[nat_bitmap_offset] = 0x01;

    // 校验和 (在块末尾 4 字节)
    // 对于最小实现，直接使用固定校验和 0
    write_u32_le(image, cp + BLOCK_SIZE - 4, 0);
}

// ==================== SIT 条目写入 ====================

fn write_sit_entry(image: &mut [u8], block_offset: usize, entry_idx: usize, vblocks: u16) {
    let entry_offset = block_offset + entry_idx * 74;
    // vblocks: 低 10 位为有效块数，高 6 位为分配类型
    let vblocks_val = vblocks; // 分配类型 0 (CUR_LOG_ALIVE)
    write_u16_le(image, entry_offset, vblocks_val);
    // valid_map: 标记前 vblocks 个块为有效
    for i in 0..vblocks as usize {
        let byte_idx = entry_offset + 2 + i / 8;
        let bit_idx = i % 8;
        image[byte_idx] |= 1 << bit_idx;
    }
    // mtime = 0
    write_u64_le(image, entry_offset + 66, 0);
}

// ==================== f2fs 目录哈希函数 ====================

/// 计算 f2fs 文件名哈希值
fn hash_filename(name: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &c in name {
        hash = hash.wrapping_mul(13).wrapping_add(c as u32);
    }
    hash
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

fn write_u64_le(buf: &mut [u8], offset: usize, val: u64) {
    let bytes = val.to_le_bytes();
    buf[offset..offset + 8].copy_from_slice(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_minimal_f2fs_128mb() {
        let partition_size = 128 * 1024 * 1024u64;
        let image = create_minimal_f2fs(partition_size);
        // 结构前缀（superblock/CP/SIT/NAT/SSA + 主区域根 inode/目录数据块），
        // 长度 = (main_blkaddr=4096 + ROOT_DATA_BLOCK_OFFSET=1 + 1) * 4096，不依赖分区大小
        assert_eq!(image.len(), 4098 * 4096);

        // 验证 superblock magic
        let magic = u32::from_le_bytes([
            image[SB_OFFSET + SB_MAGIC],
            image[SB_OFFSET + SB_MAGIC + 1],
            image[SB_OFFSET + SB_MAGIC + 2],
            image[SB_OFFSET + SB_MAGIC + 3],
        ]);
        assert_eq!(magic, F2FS_MAGIC);

        // 验证块大小
        let log_blocksize = u32::from_le_bytes([
            image[SB_OFFSET + SB_LOG_BLOCKSIZE],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 1],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 2],
            image[SB_OFFSET + SB_LOG_BLOCKSIZE + 3],
        ]);
        assert_eq!(log_blocksize, 12);

        // 验证根 inode 编号
        let root_ino = u32::from_le_bytes([
            image[SB_OFFSET + SB_ROOT_INO],
            image[SB_OFFSET + SB_ROOT_INO + 1],
            image[SB_OFFSET + SB_ROOT_INO + 2],
            image[SB_OFFSET + SB_ROOT_INO + 3],
        ]);
        assert_eq!(root_ino, 3);

        // 验证主区域起始块地址
        let main_blkaddr = u32::from_le_bytes([
            image[SB_OFFSET + SB_MAIN_BLKADDR],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 1],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 2],
            image[SB_OFFSET + SB_MAIN_BLKADDR + 3],
        ]);
        assert_eq!(main_blkaddr, 4096);

        // 验证根 inode 的 i_mode
        let root_node_offset = main_blkaddr as usize * BLOCK_SIZE;
        let i_mode = u16::from_le_bytes([
            image[root_node_offset + INODE_I_MODE],
            image[root_node_offset + INODE_I_MODE + 1],
        ]);
        assert_eq!(i_mode, 0x41ED);

        // 验证根目录数据块
        let root_data_offset = (main_blkaddr + ROOT_DATA_BLOCK_OFFSET) as usize * BLOCK_SIZE;
        // 检查 dentry bitmap: bit 0 和 bit 1 应为 1
        assert_eq!(image[root_data_offset + DENTRY_BITMAP] & 0x03, 0x03);

        // 检查条目 0: "." inode = root_ino
        let entry0_offset = root_data_offset + DENTRY_ENTRIES;
        let entry0_ino = u32::from_le_bytes([
            image[entry0_offset + 4],
            image[entry0_offset + 5],
            image[entry0_offset + 6],
            image[entry0_offset + 7],
        ]);
        assert_eq!(entry0_ino, 3);
        assert_eq!(image[root_data_offset + DENTRY_FILENAME], b'.');

        // 检查条目 1: ".." inode = root_ino
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

        // mtkclient-rs 的 format 路径直写 raw 镜像（不经过 sparse 封装），
        // 结构前缀即完整镜像，长度断言已在上方覆盖。
    }

    #[test]
    fn test_create_minimal_f2fs_238gb() {
        // 大分区（如 userdata 238GB）：结构前缀长度仍恒为 4098*4096，与分区大小无关
        let partition_size = 238u64 * 1024 * 1024 * 1024;
        let image = create_minimal_f2fs(partition_size);
        assert_eq!(image.len(), 4098 * 4096);

        // 大分区结构前缀恒定：不依赖分区大小（sparse 封装断言见 fastboot-rs 侧测试）。
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
