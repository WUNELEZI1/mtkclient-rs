use super::Explorer;
use super::ext4::{
    DirEntry, Ext4Superblock, ROOT_INODE, get_inode_mode, parse_dir, read_ext4_superblock,
    read_inode, read_inode_data, resolve_path,
};
use super::lp::LpTranslator;
use super::{LP_ATTR_READONLY, LP_ATTR_SLOT_SUFFIXED, fmt_size, is_dir_mode};

impl Explorer {
    /// cd <partition>[/path] — 切换当前分区/目录
    pub(crate) fn cmd_cd<F>(&mut self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let trimmed = target.trim();
        if trimmed.is_empty() || trimmed == "/" || trimmed == "super" {
            self.current_partition = None;
            self.current_path.clear();
            return Ok(());
        }

        // 处理 ".."：从当前路径回退一级
        if trimmed == ".." {
            if let Some(ref _part) = self.current_partition {
                if self.current_path.is_empty() {
                    // 已在分区根目录，回到 super 根
                    self.current_partition = None;
                } else if let Some(last_slash) = self.current_path.rfind('/') {
                    self.current_path = self.current_path[..last_slash].to_string();
                } else {
                    self.current_path.clear();
                }
            }
            return Ok(());
        }

        // 绝对路径：以 / 开头
        if trimmed.starts_with('/') {
            let target = trimmed.trim_start_matches('/');
            let (part_name, sub_path) = self.split_target(target)?;
            if part_name.is_empty() {
                self.current_partition = None;
                self.current_path.clear();
                return Ok(());
            }
            // 获取实际分区名
            let part_idx = self.find_partition(&part_name)?;
            let real_name = self.partitions[part_idx].name.clone();

            // 验证子路径是目录
            if !sub_path.is_empty() {
                let lp = self.make_translator(part_idx)?;
                let sb = read_ext4_superblock(read_fn, &lp)?;
                let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
                let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
                let mode = get_inode_mode(&inode_data);
                if !is_dir_mode(mode) {
                    return Err("不是目录".into());
                }
            }

            self.current_partition = Some(real_name);
            self.current_path = sub_path;
            return Ok(());
        }

        // 相对路径：当前已在某分区内时，优先尝试子目录
        if let Some(ref cur_part) = self.current_partition {
            let part_idx = self.find_partition(cur_part)?;
            let real_name = self.partitions[part_idx].name.clone();
            let lp = self.make_translator(part_idx)?;
            let sb = read_ext4_superblock(read_fn, &lp)?;

            // 先尝试将 target 当作当前目录下的子路径
            let try_path = if self.current_path.is_empty() {
                trimmed.to_string()
            } else {
                format!("{}/{}", self.current_path, trimmed)
            };

            // 尝试解析子路径
            match resolve_path(read_fn, &lp, &sb, ROOT_INODE, &try_path) {
                Ok(inode_num) => {
                    let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
                    let mode = get_inode_mode(&inode_data);
                    if is_dir_mode(mode) {
                        self.current_partition = Some(real_name);
                        self.current_path = try_path;
                        return Ok(());
                    } else {
                        return Err("不是目录".into());
                    }
                }
                Err(_) => {
                    // 子路径不存在，尝试切换到其他分区
                }
            }
        }

        // 切换分区（可能带子路径）
        let (part_name, sub_path) = self.split_target(trimmed)?;
        if part_name.is_empty() {
            self.current_partition = None;
            self.current_path.clear();
            return Ok(());
        }

        let part_idx = self.find_partition(&part_name)?;
        let real_name = self.partitions[part_idx].name.clone();

        // 验证子路径是目录
        if !sub_path.is_empty() {
            let lp = self.make_translator(part_idx)?;
            let sb = read_ext4_superblock(read_fn, &lp)?;
            let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
            let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
            let mode = get_inode_mode(&inode_data);
            if !is_dir_mode(mode) {
                return Err("不是目录".into());
            }
        }

        self.current_partition = Some(real_name);
        self.current_path = sub_path;
        Ok(())
    }

    /// pwd — 显示当前路径
    pub(crate) fn cmd_pwd(&self) {
        match &self.current_partition {
            None => println!("/"),
            Some(part) => {
                if self.current_path.is_empty() {
                    println!("/{}/", part);
                } else {
                    println!("/{}/{}", part, self.current_path);
                }
            }
        }
    }

    /// tree <part>[/path] [depth] — 树形显示
    pub(crate) fn cmd_tree<F>(&self, read_fn: &mut F, arg: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let parts: Vec<&str> = arg.splitn(2, ' ').collect();
        let target = parts[0].trim();
        let max_depth: usize = if parts.len() > 1 {
            parts[1].trim().parse().unwrap_or(3)
        } else {
            3
        };

        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() {
            return Err("用法: tree <partition>[/path] [depth]".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let start_inode = if sub_path.is_empty() {
            ROOT_INODE
        } else {
            resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?
        };

        println!("{}", part_name);
        self.tree_recurse(read_fn, &lp, &sb, start_inode, "", max_depth, max_depth)
    }

    fn tree_recurse<F>(
        &self,
        read_fn: &mut F,
        lp: &LpTranslator,
        sb: &Ext4Superblock,
        inode_num: u32,
        prefix: &str,
        depth: usize,
        max_depth: usize,
    ) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        if depth == 0 {
            return Ok(());
        }

        let inode_data = read_inode(read_fn, lp, sb, inode_num)?;
        let dir_data = match read_inode_data(read_fn, lp, sb, &inode_data) {
            Ok(d) => d,
            Err(e) => {
                // 非 extent inode / 旧式 ext 格式等：跳过不崩溃
                println!("{}(读取失败: {})", prefix, e);
                return Ok(());
            }
        };
        let entries = parse_dir(&dir_data);

        // 分离目录和文件，排序
        let mut items: Vec<&DirEntry> = entries
            .iter()
            .filter(|e| e.inode != 0 && e.name != "." && e.name != "..")
            .collect();
        items.sort_by(|a, b| {
            // 目录优先
            let a_is_dir = a.file_type == 2;
            let b_is_dir = b.file_type == 2;
            if a_is_dir != b_is_dir {
                return b_is_dir.cmp(&a_is_dir);
            }
            a.name.cmp(&b.name)
        });

        let last_idx = items.len().saturating_sub(1);
        for (i, entry) in items.iter().enumerate() {
            let is_last = i == last_idx;
            let connector = if is_last { "+-- " } else { "|-- " };
            let name_suffix = if entry.file_type == 2 { "/" } else { "" };
            println!("{}{}{}{}", prefix, connector, entry.name, name_suffix);

            if entry.file_type == 2 {
                let new_prefix = if is_last {
                    format!("{}    ", prefix)
                } else {
                    format!("{}|   ", prefix)
                };
                self.tree_recurse(
                    read_fn,
                    lp,
                    sb,
                    entry.inode,
                    &new_prefix,
                    depth - 1,
                    max_depth,
                )?;
            }
        }
        Ok(())
    }

    /// info <partition> — 分区详情
    pub(crate) fn cmd_info<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let part_idx = self.find_partition(target)?;
        let part = &self.partitions[part_idx];
        let total_size = self.partition_size(part_idx);

        println!("分区名: {}", part.name);
        println!("大小:   {} ({} bytes)", fmt_size(total_size), total_size);

        let mut tags = Vec::new();
        if (part.attributes & LP_ATTR_READONLY) != 0 {
            tags.push("READONLY");
        }
        if (part.attributes & LP_ATTR_SLOT_SUFFIXED) != 0 {
            if part.name.ends_with("_a") {
                tags.push("[A-slot]");
            } else if part.name.ends_with("_b") {
                tags.push("[B-slot]");
            }
        }
        println!(
            "标记:   {}",
            if tags.is_empty() {
                "无".to_string()
            } else {
                tags.join(" ")
            }
        );
        println!("Extent:  {} 段", part.num_extents);

        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                println!(
                    "  [{}/{}] offset=0x{:08X} size={}",
                    i + 1,
                    part.num_extents,
                    ext.phys_offset,
                    fmt_size(ext.size),
                );
            }
        }

        // 尝试读 ext4 superblock
        let lp = self.make_translator(part_idx)?;
        match read_ext4_superblock(read_fn, &lp) {
            Ok(sb) => {
                println!();
                println!("文件系统: ext4");
                println!("块大小:   {} bytes", sb.block_size);
                println!("Inode/组: {}", sb.inodes_per_group);
                println!("Inode大小: {} bytes", sb.inode_size);
            }
            Err(_) => {
                println!();
                println!("文件系统: 未知（无法读取 ext4 superblock）");
            }
        }

        Ok(())
    }
}
