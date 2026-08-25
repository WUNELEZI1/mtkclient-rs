use super::Explorer;
use super::ext4::{
    get_inode_mode, get_inode_size, read_ext4_superblock, read_inode, read_inode_data, resolve_path,
};
use super::is_dir_mode;
use super::lp::{LpTranslator, read_lp_metadata};

/// fs_shell 入口函数
pub fn run_explorer<F>(read_fn: &mut F) -> Result<(), String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // banner 在 run_interactive 中打印，此处不再重复

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

/// 从 ext4 分区中按路径读取文件内容（不依赖 LP 元数据）
///
/// 复用 fs_shell 内部的 ext4 解析逻辑（支持 BGD 32/64 字节、inline symlink、
/// 空 inode 等），使用 identity translator 直接按分区偏移读取。
///
/// 参数：
/// - `read_fn`: 读取回调 `(offset, len) -> Result<Vec<u8>, String>`，offset 从分区起始算
/// - `partition_size`: 分区总大小
/// - `path`: 文件路径，如 `"system/build.prop"`
///
/// 返回：文件原始数据
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
// 公共 API：通过 LP translator 从 super 分区读取文件
// =============================================================================

/// 通过 LP metadata 从 super 分区内的逻辑分区读取文件
///
/// 与 fs_shell 的 `cat <super_part>/<partition>/<path>` 完全相同的代码路径。
///
/// # 参数
/// - `super_read_fn`: 从 super 分区起始读取数据的函数
/// - `target_partition`: 目标逻辑分区名（如 "system_b"）
/// - `file_path`: ext4 内文件路径（如 "system/build.prop"）
///
/// # 返回
/// 文件内容字节
pub fn read_file_via_lp<F>(
    super_read_fn: &mut F,
    target_partition: &str,
    file_path: &str,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
{
    // Step 1: 从 super 读取 LP metadata
    let (partitions, _all_extents) = read_lp_metadata(super_read_fn)?;

    // Step 2: 查找目标逻辑分区
    let lp_part = partitions
        .iter()
        .find(|p| p.name == target_partition)
        .ok_or_else(|| format!("LP 中未找到分区: {}", target_partition))?;

    // 从 all_extents 中获取该分区的 extents
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

    // Step 3: 创建 LP translator（从分区的 extents）
    let lp_translator = LpTranslator {
        extents: part_extents,
        total_size: part_size,
    };

    // Step 4: 读取 ext4 superblock
    let sb = read_ext4_superblock(super_read_fn, &lp_translator)?;

    log::info!(
        "[explorer] ext4: block_size={}, inode_size={}, bgd_entry_size={}",
        sb.block_size,
        sb.inode_size,
        sb.bgd_entry_size
    );

    // Step 5: 解析路径并读取文件
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
