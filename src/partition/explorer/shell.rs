use crate::color::Colorize;
use super::lp::{LpExtent, LpPartition, LpTranslator};
use super::ext4::{
    DirEntry, read_ext4_superblock, read_inode, get_inode_size, get_inode_mode, get_inode_flags,
    read_inode_data, parse_dir, resolve_path, ROOT_INODE, EXT4_INODE_FLAG_EXTENTS,
};
use super::{
    LP_ATTR_READONLY, LP_ATTR_SLOT_SUFFIXED, fmt_size, fmt_inode_size, file_type_str,
    is_file_mode, is_dir_mode,
};

pub struct Explorer {
    pub(crate) partitions: Vec<LpPartition>,
    pub(crate) all_extents: Vec<LpExtent>,
    pub(crate) current_partition: Option<String>,
    pub(crate) current_path: String,
}

impl Explorer {
    /// 按名称查找分区，自动补 _b 后缀
    pub(crate) fn find_partition(&self, name: &str) -> Result<usize, String> {
        // 精确匹配
        for (i, p) in self.partitions.iter().enumerate() {
            if p.name == name {
                return Ok(i);
            }
        }
        // 尝试 _b / _a 后缀
        for suffix in &["_b", "_a"] {
            let candidate = format!("{}{}", name, suffix);
            for (i, p) in self.partitions.iter().enumerate() {
                if p.name == candidate {
                    return Ok(i);
                }
            }
        }
        Err(format!("未找到分区: {}", name))
    }

    /// 构造指定分区的 LpTranslator
    pub(crate) fn make_translator(&self, part_idx: usize) -> Result<LpTranslator, String> {
        let part = &self.partitions[part_idx];
        let mut extents = Vec::new();
        let mut total_size = 0u64;
        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                extents.push(ext.clone());
                total_size += ext.size;
            }
        }
        Ok(LpTranslator {
            extents,
            total_size,
        })
    }

    /// 计算分区总大小
    pub(crate) fn partition_size(&self, part_idx: usize) -> u64 {
        let part = &self.partitions[part_idx];
        let mut total = 0u64;
        for i in 0..part.num_extents {
            let ext_idx = part.first_extent_index + i;
            if let Some(ext) = self.all_extents.get(ext_idx as usize) {
                total += ext.size;
            }
        }
        total
    }

    /// 拆分 target: "system_b/system/etc" → ("system_b", "system/etc")
    pub(crate) fn split_target(&self, target: &str) -> Result<(String, String), String> {
        let target = target.trim_start_matches('/');
        if target.is_empty() {
            return Ok(("".to_string(), "".to_string()));
        }
        // 尝试匹配分区名（第一段或前两段）
        // 先尝试完整第一段作为分区名
        let first_slash = target.find('/');
        let first_seg = if let Some(pos) = first_slash {
            &target[..pos]
        } else {
            return Ok((target.to_string(), "".to_string()));
        };

        // 精确匹配
        if self.find_partition(first_seg).is_ok() {
            Ok((
                first_seg.to_string(),
                target[first_slash.unwrap() + 1..].to_string(),
            ))
        } else {
            Err(format!("未找到分区: {}", first_seg))
        }
    }
}

impl Explorer {
    /// ls super — 列出所有 LP 分区
    fn cmd_ls_super(&self) {
        println!("{:<20} {:>12} {:>12}  {}", "分区", "偏移", "大小", "标记");
        println!("{}", "-".repeat(60));
        for part in &self.partitions {
            let (mut total_size, mut first_offset) = (0u64, 0u64);
            let mut first = true;
            for i in 0..part.num_extents {
                if let Some(ext) = self.all_extents.get((part.first_extent_index + i) as usize) {
                    if first {
                        first_offset = ext.phys_offset;
                        first = false;
                    }
                    total_size += ext.size;
                }
            }
            let mut tags = Vec::new();
            if (part.attributes & LP_ATTR_READONLY) != 0 {
                tags.push("RO");
            }
            if (part.attributes & LP_ATTR_SLOT_SUFFIXED) != 0 {
                if part.name.ends_with("_a") {
                    tags.push("[A-slot]");
                } else if part.name.ends_with("_b") {
                    tags.push("[B-slot]");
                }
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                tags.join(" ")
            };
            println!(
                "{:<20} 0x{:>08X} {:>12}  {}",
                part.name,
                first_offset,
                fmt_size(total_size),
                tag_str,
            );
        }
    }

    /// ls [partition][/path] — 列出目录内容
    pub(crate) fn cmd_ls<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let trimmed = target.trim();
        if trimmed.is_empty() || trimmed == "super" {
            if self.current_partition.is_none() {
                self.cmd_ls_super();
                return Ok(());
            }
            // 有当前分区时，列出当前路径
            let part_name = self.current_partition.as_ref().unwrap().clone();
            let sub_path = self.current_path.clone();
            // 构造完整 target: part_name/sub_path
            let full_target = if sub_path.is_empty() {
                part_name
            } else {
                format!("{}/{}", part_name, sub_path)
            };
            return self.cmd_ls_path(read_fn, &full_target, "");
        }

        // 处理相对路径：不以 / 开头且当前在分区内
        if !trimmed.starts_with('/') {
            if let Some(ref cur_part) = self.current_partition {
                // 先检查是否是分区名
                if self.find_partition(trimmed).is_err() {
                    // 不是分区名，拼接当前路径
                    let full_target = if self.current_path.is_empty() {
                        format!("{}", cur_part)
                    } else {
                        format!("{}/{}", cur_part, self.current_path)
                    };
                    // 拼接 target
                    let full_target = if trimmed.is_empty() {
                        full_target
                    } else {
                        format!("{}/{}", full_target, trimmed)
                    };
                    return self.cmd_ls_path(read_fn, &full_target, "");
                }
            }
        }

        self.cmd_ls_path(read_fn, target, "")
    }

    fn cmd_ls_path<F>(&self, read_fn: &mut F, target: &str, _unused: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() {
            self.cmd_ls_super();
            return Ok(());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let dir_inode = if sub_path.is_empty() {
            ROOT_INODE
        } else {
            resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?
        };

        let inode_data = read_inode(read_fn, &lp, &sb, dir_inode)?;
        let mode = get_inode_mode(&inode_data);
        let size = get_inode_size(&inode_data);

        if is_file_mode(mode) {
            // 单文件：显示文件信息
            let mut ft = "FILE".to_string();
            if (get_inode_flags(&inode_data) & EXT4_INODE_FLAG_EXTENTS) != 0 {
                ft = "FILE(ext)".to_string();
            }
            println!("{:<40} {:>10}  {}", part_name, fmt_inode_size(size), ft);
            return Ok(());
        }

        // 目录
        let dir_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
        let entries = parse_dir(&dir_data);

        // 分离目录和文件
        let mut dirs: Vec<&DirEntry> = Vec::new();
        let mut files: Vec<&DirEntry> = Vec::new();
        for e in &entries {
            if e.name == "." || e.name == ".." {
                continue;
            }
            match e.file_type {
                2 => dirs.push(e),
                _ => files.push(e),
            }
        }
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));

        // 获取每个条目的大小（需要读 inode）
        for d in &dirs {
            match read_inode(read_fn, &lp, &sb, d.inode) {
                Ok(ino_data) => {
                    let ino_size = get_inode_size(&ino_data);
                    // inode 数据损坏时大小可能离奇，限制显示
                    let display_size = if ino_size > 0x1_0000_0000 {
                        4096u64
                    } else {
                        ino_size
                    };
                    println!(
                        "{:<40} {:>10}  {}",
                        format!("{}/", d.name),
                        fmt_inode_size(display_size),
                        file_type_str(d.file_type).dimmed(),
                    );
                }
                Err(_) => {
                    println!(
                        "{:<40} {:>10}  {}",
                        format!("{}/", d.name),
                        "4K",
                        file_type_str(d.file_type).dimmed(),
                    );
                }
            }
        }
        for f in &files {
            match read_inode(read_fn, &lp, &sb, f.inode) {
                Ok(ino_data) => {
                    let ino_size = get_inode_size(&ino_data);
                    // inode 数据损坏时大小可能离奇，限制显示
                    let display_size = if ino_size > 0x1_0000_0000 {
                        4096u64
                    } else {
                        ino_size
                    };
                    println!(
                        "{:<40} {:>10}  {}",
                        f.name,
                        fmt_inode_size(display_size),
                        file_type_str(f.file_type).dimmed(),
                    );
                }
                Err(_) => {
                    println!(
                        "{:<40} {:>10}  {}",
                        f.name,
                        "???",
                        file_type_str(f.file_type).dimmed(),
                    );
                }
            }
        }

        println!(
            "\n共 {} 项 ({} 目录, {} 文件)",
            dirs.len() + files.len(),
            dirs.len(),
            files.len()
        );
        Ok(())
    }

    /// cat <partition/path> — 显示文件内容
    pub(crate) fn cmd_cat<F>(&self, read_fn: &mut F, target: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        let (part_name, sub_path) = self.split_target(target)?;
        if part_name.is_empty() || sub_path.is_empty() {
            return Err("用法: cat <partition>/<path>".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
        let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
        let mode = get_inode_mode(&inode_data);
        let size = get_inode_size(&inode_data);

        if is_dir_mode(mode) {
            // 目录 → 列出条目
            let dir_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
            let entries = parse_dir(&dir_data);
            println!("(目录) {} 条目:", entries.len());
            for e in &entries {
                if e.inode != 0 {
                    let ino_data = read_inode(read_fn, &lp, &sb, e.inode)?;
                    let ino_size = get_inode_size(&ino_data);
                    println!(
                        "  {}  {:>10}  {}",
                        file_type_str(e.file_type),
                        fmt_inode_size(ino_size),
                        e.name,
                    );
                }
            }
            return Ok(());
        }

        // 文件
        let file_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
        println!("[{}, {} bytes]", part_name, size);

        match String::from_utf8(file_data.clone()) {
            Ok(text) => {
                for line in text.lines() {
                    println!(" {}", line);
                }
            }
            Err(_) => {
                println!("[binary, {} bytes]", file_data.len());
                // hex dump 前 512 字节
                let dump_len = 512.min(file_data.len());
                for row in 0..dump_len / 16 {
                    let off = row * 16;
                    let hex: Vec<String> = file_data[off..off + 16]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect();
                    let ascii: String = file_data[off..off + 16]
                        .iter()
                        .map(|&b| {
                            if b >= 0x20 && b < 0x7F {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    println!("  {:08X}: {}  {}", off, hex.join(" "), ascii);
                }
            }
        }
        Ok(())
    }
}
