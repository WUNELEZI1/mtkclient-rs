use super::Explorer;
use super::ext4::{
    get_inode_mode, get_inode_size, read_ext4_superblock, read_inode, read_inode_data, resolve_path,
};
use super::is_dir_mode;
use super::lp::{LpTranslator, read_lp_metadata};

/// fs_shell entry function
pub fn run_explorer<F>(read_fn: &mut F) -> Result<(), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // The banner is printed in run_interactive, so it is not repeated here

    let (partitions, extents) = read_lp_metadata(read_fn)?;
    println!("解析到 {} 个逻辑分区:", partitions.len());
    for p in &partitions {
        println!("  {}", p.name);
    }
    println!();

    let mut explorer = Explorer {
        partitions,
        all_extents: extents,
        current_partition: None,
        current_path: String::new(),
    };

    explorer.run_interactive(read_fn);
    Ok(())
}

/// Read file content from an ext4 partition by path (independent of LP metadata).
///
/// Reuses fs_shell's internal ext4 parsing logic (supports 32/64-byte BGD, inline symlinks,
/// empty inodes, etc.) and uses an identity translator to read directly at partition offsets.
///
/// Parameters:
/// - `read_fn`: read callback `(offset, len) -> Result<Vec<u8>, String>`, offset relative to partition start
/// - `partition_size`: total partition size
/// - `path`: file path, e.g. `"system/build.prop"`
///
/// Returns: raw file data
pub fn read_file_by_path<F>(
    read_fn: &mut F,
    partition_size: u64,
    path: &str,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    let lp = LpTranslator::identity(partition_size);
    let sb = read_ext4_superblock(read_fn, &lp)?;

    log::info!(
        "[explorer] ext4: block_size={}, inode_size={}, bgd_entry_size={}",
        sb.block_size,
        sb.inode_size,
        sb.bgd_entry_size
    );

    let inode_num = resolve_path(read_fn, &lp, &sb, super::ext4::ROOT_INODE, path)?;
    log::info!("[explorer] 找到 {} -> inode={}", path, inode_num);

    let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
    let mode = get_inode_mode(&inode_data);
    let size = get_inode_size(&inode_data);

    if is_dir_mode(mode) {
        return Err(format!("{} 是目录，不是文件", path));
    }

    log::info!(
        "[explorer] 读取 {} ({} bytes, mode=0x{:04X})",
        path,
        size,
        mode
    );
    let data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;

    Ok(data)
}

// =============================================================================
// Public API: read files from the super partition via the LP translator
// =============================================================================

/// Read a file from a logical partition inside super via LP metadata.
///
/// Uses exactly the same code path as fs_shell's `cat <super_part>/<partition>/<path>`.
///
/// # Parameters
/// - `super_read_fn`: function that reads data from the start of the super partition
/// - `target_partition`: target logical partition name (e.g. "system_b")
/// - `file_path`: file path inside ext4 (e.g. "system/build.prop")
///
/// # Returns
/// File content bytes
pub fn read_file_via_lp<F>(
    super_read_fn: &mut F,
    target_partition: &str,
    file_path: &str,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // Step 1: read LP metadata from super
    let (partitions, _all_extents) = read_lp_metadata(super_read_fn)?;

    // Step 2: locate the target logical partition
    let lp_part = partitions
        .iter()
        .find(|p| p.name == target_partition)
        .ok_or_else(|| format!("LP 中未找到分区: {}", target_partition))?;

    // Collect this partition's extents from all_extents
    let start = lp_part.first_extent_index as usize;
    let count = lp_part.num_extents as usize;
    let part_extents = if start + count <= _all_extents.len() {
        _all_extents[start..start + count].to_vec()
    } else {
        return Err(format!(
            "LP 分区 {} extents 索引越界: index={}, count={}, total={}",
            target_partition,
            start,
            count,
            _all_extents.len()
        ));
    };
    let part_size: u64 = part_extents.iter().map(|e| e.size).sum();

    log::info!(
        "[explorer] LP 分区 {}: {} extents, size=0x{:X}",
        target_partition,
        part_extents.len(),
        part_size
    );

    // Step 3: build an LP translator from the partition's extents
    let lp_translator = LpTranslator {
        extents: part_extents,
        total_size: part_size,
    };

    // Step 4: read the ext4 superblock
    let sb = read_ext4_superblock(super_read_fn, &lp_translator)?;

    log::info!(
        "[explorer] ext4: block_size={}, inode_size={}, bgd_entry_size={}",
        sb.block_size,
        sb.inode_size,
        sb.bgd_entry_size
    );

    // Step 5: resolve the path and read the file
    let inode_num = resolve_path(
        super_read_fn,
        &lp_translator,
        &sb,
        super::ext4::ROOT_INODE,
        file_path,
    )?;

    let inode_data = read_inode(super_read_fn, &lp_translator, &sb, inode_num)?;
    let mode = get_inode_mode(&inode_data);
    let size = get_inode_size(&inode_data);

    if is_dir_mode(mode) {
        return Err(format!("{} 是目录，不是文件", file_path));
    }

    log::info!(
        "[explorer] 找到 {}/{} -> inode={}, size={}",
        target_partition,
        file_path,
        inode_num,
        size
    );

    let data = read_inode_data(super_read_fn, &lp_translator, &sb, &inode_data)?;

    Ok(data)
}
