use super::Explorer;
use crate::color::Colorize;
use std::fs;
use super::lp::LpTranslator;
use super::ext4::{
    Ext4Superblock, read_ext4_superblock, read_inode, get_inode_mode, read_inode_data, parse_dir,
    resolve_path, ROOT_INODE,
};
use super::is_dir_mode;

impl Explorer {
    /// cp [-r] [--depth N] <partition/path> <local_path> — 提取到本地
    ///   -r          递归复制目录（默认只复制文件，目录会报错）
    ///   --depth N   限制递归深度（1=只复制当前目录文件，2=包含子目录，等）
    fn cmd_cp<F>(&self, read_fn: &mut F, arg: &str) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        // 解析参数
        let tokens: Vec<&str> = arg.split_whitespace().collect();
        let mut recursive = false;
        let mut max_depth: Option<usize> = None;
        let mut src_idx = 0usize;
        let mut dst_idx = tokens.len().saturating_sub(1);

        let mut i = 0;
        while i < tokens.len() {
            match tokens[i] {
                "-r" | "-R" | "--recursive" => {
                    recursive = true;
                    i += 1;
                }
                "-d" | "--depth" => {
                    if i + 1 < tokens.len() {
                        max_depth = tokens[i + 1].parse().ok();
                        i += 2;
                    } else {
                        return Err("--depth 后缺少数值".into());
                    }
                }
                _ => {
                    if src_idx == 0 && !tokens[i].starts_with('-') {
                        src_idx = i;
                    }
                    dst_idx = i;
                    i += 1;
                }
            }
        }

        if src_idx >= tokens.len() || dst_idx >= tokens.len() || src_idx == dst_idx {
            return Err("用法: cp [-r] [--depth N] <partition/path> <local_path>".into());
        }

        let src_raw = tokens[src_idx];
        let dst_raw = tokens[dst_idx];

        let (part_name, sub_path) = self.split_target(src_raw)?;
        if part_name.is_empty() || sub_path.is_empty() {
            return Err("用法: cp [-r] [--depth N] <partition/path> <local_path>".into());
        }

        let part_idx = self.find_partition(&part_name)?;
        let lp = self.make_translator(part_idx)?;
        let sb = read_ext4_superblock(read_fn, &lp)?;

        let inode_num = resolve_path(read_fn, &lp, &sb, ROOT_INODE, &sub_path)?;
        let inode_data = read_inode(read_fn, &lp, &sb, inode_num)?;
        let mode = get_inode_mode(&inode_data);

        // 构造目标路径
        let dst = if dst_raw.ends_with('\\') || dst_raw.ends_with('/') {
            // 目标是目录
            let file_name = sub_path.rsplit('/').next().unwrap_or("unknown");
            let sep = if dst_raw.ends_with('\\') { "\\" } else { "/" };
            format!("{}{}{}", dst_raw, sep, file_name)
        } else {
            dst_raw.to_string()
        };

        if is_dir_mode(mode) {
            if !recursive {
                return Err(format!("{} 是目录，请使用 -r 选项递归复制", src_raw));
            }
            // 递归复制目录，应用 max_depth
            let effective_max = max_depth.unwrap_or(usize::MAX);
            println!(
                "递归复制: {} → {} (max_depth={})",
                src_raw, dst, effective_max
            );
            self.cp_recurse_depth(
                read_fn,
                &lp,
                &sb,
                inode_num,
                &dst,
                &sub_path,
                0,
                effective_max,
            )?;
        } else {
            // 复制文件
            println!("提取: {} → {}", src_raw, dst);
            let file_data = read_inode_data(read_fn, &lp, &sb, &inode_data)?;
            fs::write(&dst, &file_data).map_err(|e| format!("写入失败: {}", e))?;
            println!("完成: {} ({} bytes)", dst, file_data.len());
        }

        Ok(())
    }

    fn cp_recurse_depth<F>(
        &self,
        read_fn: &mut F,
        lp: &LpTranslator,
        sb: &Ext4Superblock,
        inode_num: u32,
        dst_dir: &str,
        src_prefix: &str,
        depth: usize,
        max_depth: usize,
    ) -> Result<(), String>
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        // 达到用户指定的最大深度时停止递归，创建空目录作为占位
        if depth >= max_depth {
            if max_depth < usize::MAX {
                println!(
                    "  [depth={}/{}] 达到最大深度，跳过子目录: {}",
                    depth, max_depth, src_prefix
                );
            }
            let _ = fs::create_dir_all(dst_dir);
            return Ok(());
        }

        // 硬编码安全上限：ext4 最大深度约 4000+
        const MAX_DEPTH_HARD: usize = 256;
        if depth > MAX_DEPTH_HARD {
            eprintln!(
                "  [跳过] 目录过深 (>{}) 跳过递归: {}",
                MAX_DEPTH_HARD, src_prefix
            );
            let _ = fs::create_dir_all(dst_dir);
            return Ok(());
        }

        if let Err(e) = fs::create_dir_all(dst_dir) {
            eprintln!("  [警告] 创建目录失败 {}: {}", dst_dir, e);
            return Ok(());
        }

        let inode_data = match read_inode(read_fn, lp, sb, inode_num) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [跳过] 读取 inode {} 失败: {}", inode_num, e);
                return Ok(());
            }
        };

        let dir_data = match read_inode_data(read_fn, lp, sb, &inode_data) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [跳过] 读取目录数据失败 {}: {}", src_prefix, e);
                return Ok(());
            }
        };

        let entries = parse_dir(&dir_data);

        for entry in &entries {
            if entry.inode == 0 || entry.name == "." || entry.name == ".." {
                continue;
            }

            let entry_path = format!("{}/{}", src_prefix, entry.name);
            let dst_path = format!("{}/{}", dst_dir, entry.name);

            let entry_inode_data = match read_inode(read_fn, lp, sb, entry.inode) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "  [跳过] 无法读取 {} (inode={}): {}",
                        entry_path, entry.inode, e
                    );
                    // 创建空文件作为占位
                    let _ = fs::write(&dst_path, b"");
                    continue;
                }
            };

            let mode = get_inode_mode(&entry_inode_data);

            if is_dir_mode(mode) {
                if let Err(e) = self.cp_recurse_depth(
                    read_fn,
                    lp,
                    sb,
                    entry.inode,
                    &dst_path,
                    &entry_path,
                    depth + 1,
                    max_depth,
                ) {
                    eprintln!("  [警告] 子目录复制失败 {}: {}", entry_path, e);
                }
            } else {
                match read_inode_data(read_fn, lp, sb, &entry_inode_data) {
                    Ok(file_data) => {
                        if let Err(e) = fs::write(&dst_path, &file_data) {
                            eprintln!("  [跳过] 写入失败 {}: {}", dst_path, e);
                            // 创建空文件作为占位
                            let _ = fs::write(&dst_path, b"");
                        } else {
                            println!("  提取: {} ({} bytes)", entry_path, file_data.len());
                        }
                    }
                    Err(e) => {
                        eprintln!("  [跳过] 读取文件失败 {}: {}", entry_path, e);
                        // 创建空文件作为占位
                        let _ = fs::write(&dst_path, b"");
                    }
                }
            }
        }

        Ok(())
    }

    /// 帮助
    fn print_help() {
        println!("命令:");
        println!("  ls [super]                 列出 super 分区表");
        println!("  ls [part[/path]]           列出目录内容");
        println!("  cat <part/path>            显示文件内容");
        println!("  cp [-r] [--depth N] <part/path> <local>  提取文件/目录到本地");
        println!("      -r        递归复制目录");
        println!("      --depth N 限制递归深度 (1=当前目录, 2=一层子目录...)");
        println!("  cd <part>[/path]           切换当前分区/目录");
        println!("  pwd                        显示当前路径");
        println!("  tree <part>[/path] [depth] 树形显示 (默认深度3)");
        println!("  info <part>                分区详情");
        println!("  help                       帮助");
        println!("  quit / exit                退出");
    }
}

impl Explorer {
    pub fn run_interactive<F>(&mut self, read_fn: &mut F)
    where
        F: FnMut(u64, u64) -> Result<Vec<u8>, String>,
    {
        println!("\n{}", "zyb fs_shell - super.img 浏览器".bold());
        println!("输入 'help' 查看命令\n");

        loop {
            // 检查 Ctrl+C 状态
            if crate::cancel::force_requested() {
                println!("\n强制退出 fs_shell。");
                crate::cancel::reset();
                break;
            }
            if crate::cancel::requested() {
                println!("\n(已取消，输入 help 查看命令，再次 Ctrl+C 强制退出)");
                crate::cancel::reset();
                continue;
            }

            // 提示符
            match &self.current_partition {
                None => print!("{}", "zyb> ".cyan()),
                Some(part) => {
                    if self.current_path.is_empty() {
                        print!("{}", format!("zyb:/{}/> ", part).cyan());
                    } else {
                        print!(
                            "{}",
                            format!("zyb:/{}/{}> ", part, self.current_path).cyan()
                        );
                    }
                }
            };
            use std::io::Write;
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            match std::io::stdin().read_line(&mut input) {
                Ok(0) => {
                    // EOF / Ctrl+D
                    println!("\n再见!");
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    println!("读取输入失败");
                    break;
                }
            }

            // read_line 之后也检查 cancel
            if crate::cancel::force_requested() {
                println!("\n强制退出 fs_shell。");
                crate::cancel::reset();
                break;
            }
            if crate::cancel::requested() {
                crate::cancel::reset();
                continue;
            }

            let line = input.trim();
            if line.is_empty() {
                continue;
            }

            // 拆分命令和参数
            let (cmd, arg) = if let Some(pos) = line.find(' ') {
                (&line[..pos], line[pos + 1..].trim())
            } else {
                (line, "")
            };

            match cmd {
                "ls" => {
                    if let Err(e) = self.cmd_ls(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cat" => {
                    if let Err(e) = self.cmd_cat(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cp" => {
                    if let Err(e) = self.cmd_cp(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "cd" => {
                    if let Err(e) = self.cmd_cd(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "pwd" => {
                    self.cmd_pwd();
                }
                "tree" => {
                    if let Err(e) = self.cmd_tree(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "info" => {
                    if let Err(e) = self.cmd_info(read_fn, arg) {
                        println!("{}", format!("错误: {}", e).red());
                    }
                }
                "help" | "?" => {
                    Self::print_help();
                }
                "quit" | "exit" | "q" => {
                    println!("再见!");
                    break;
                }
                _ => {
                    println!(
                        "{}",
                        format!("未知命令: {} (输入 help 查看帮助)", cmd).red()
                    );
                }
            }
        }
    }
}
