//! Script mode: read a command list from a JSON file and run it sequentially
//!
//! Usage: mtkclient run <script.json>
//!
//! JSON format:
//! ```json
//! {
//!   "name": "flash script",
//!   "description": "flash all partitions in one go",
//!   "continue_on_error": true,
//!   "commands": [
//!     "w super super.img",
//!     "w boot_a boot_a.img",
//!     "w dtbo dtbo.img",
//!     "e userdata",
//!     "zyb erase_data",
//!     "reboot system"
//!   ]
//! }
//! ```
//!
//! Field reference:
//! - `name`: script name (for display)
//! - `description`: script description (for display)
//! - `continue_on_error`: whether to keep running after a command fails (default true)
//! - `commands`: command array; each element uses the same syntax as the command line

use crate::color::Colorize;
use log::{error, info};

use crate::da::DAXFlash;
use crate::system::config::AppConfig;

/// Script JSON structure
struct ScriptFile {
    name: String,
    description: String,
    continue_on_error: bool,
    commands: Vec<String>,
}

/// Parse a JSON script file (hand-rolled parser, no serde_json dependency)
fn parse_script(path: &str) -> Result<ScriptFile, Box<dyn std::error::Error>> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("无法读取脚本文件 '{}': {}", path, e))?;

    // Strip comment lines (starting with //) and trailing commas to make hand editing easier
    let cleaned: String = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Skip // comment lines (but leave // inside strings alone)
            !trimmed.starts_with("//")
        })
        .map(|line| {
            let trimmed = line.trim_end();
            // Drop trailing commas (the one before the closing ] of an array)
            if trimmed.ends_with(',') && !trimmed.contains('"') && trimmed.contains(']') {
                trimmed[..trimmed.len() - 1].to_string()
            } else {
                trimmed.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Helpers for extracting JSON values
    fn extract_string_value(json: &str, key: &str) -> Option<String> {
        let pattern = format!("\"{}\"", key);
        let start = json.find(&pattern)?;
        // Find the colon after the key
        let after_key = &json[start + pattern.len()..];
        let colon_pos = after_key.find(':')?;
        let after_colon = after_key[colon_pos + 1..].trim_start();

        if after_colon.starts_with('"') {
            // String value
            let inner = &after_colon[1..];
            let end = inner.find('"')?;
            Some(inner[..end].to_string())
        } else if after_colon.starts_with("true") {
            Some("true".to_string())
        } else if after_colon.starts_with("false") {
            Some("false".to_string())
        } else {
            None
        }
    }

    fn extract_string_array(json: &str, key: &str) -> Option<Vec<String>> {
        let pattern = format!("\"{}\"", key);
        let start = json.find(&pattern)?;
        let after_key = &json[start + pattern.len()..];
        let colon_pos = after_key.find(':')?;
        let after_colon = after_key[colon_pos + 1..].trim_start();

        if !after_colon.starts_with('[') {
            return None;
        }

        // Find the end of the array
        let inner = &after_colon[1..];
        let end = inner.find(']')?;
        let array_content = &inner[..end];

        let mut result = Vec::new();
        let mut pos = 0;
        while pos < array_content.len() {
            // Skip whitespace and commas
            while pos < array_content.len()
                && (array_content[pos..].starts_with(' ')
                    || array_content[pos..].starts_with('\t')
                    || array_content[pos..].starts_with('\n')
                    || array_content[pos..].starts_with('\r')
                    || array_content[pos..].starts_with(','))
            {
                pos += 1;
            }
            if pos >= array_content.len() {
                break;
            }

            if array_content[pos..].starts_with('"') {
                // Extract the string
                let s = &array_content[pos + 1..];
                // Handle escape sequences
                let mut decoded = String::new();
                let mut chars = s.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if let Some(next) = chars.next() {
                            match next {
                                'n' => decoded.push('\n'),
                                't' => decoded.push('\t'),
                                'r' => decoded.push('\r'),
                                '\\' => decoded.push('\\'),
                                '"' => decoded.push('"'),
                                '/' => decoded.push('/'),
                                _ => {
                                    decoded.push('\\');
                                    decoded.push(next);
                                }
                            }
                        }
                    } else if c == '"' {
                        break;
                    } else {
                        decoded.push(c);
                    }
                }
                if !decoded.is_empty() {
                    result.push(decoded);
                }
                // Continue past the closing quote
                let end_quote = array_content[pos + 1..].find('"').map(|i| pos + 1 + i + 1);
                pos = end_quote.unwrap_or(pos + 1);
            } else {
                pos += 1;
            }
        }

        Some(result)
    }

    let name = extract_string_value(&cleaned, "name").unwrap_or_default();
    let description = extract_string_value(&cleaned, "description").unwrap_or_default();
    let continue_on_error = extract_string_value(&cleaned, "continue_on_error")
        .map(|v| v == "true")
        .unwrap_or(true);
    let commands = extract_string_array(&cleaned, "commands")
        .ok_or_else(|| format!("脚本文件 '{}' 缺少 'commands' 字段或格式错误", path))?;

    if commands.is_empty() {
        return Err(format!("脚本文件 '{}' 的 'commands' 为空", path).into());
    }

    Ok(ScriptFile {
        name,
        description,
        continue_on_error,
        commands,
    })
}

/// Execute a script
pub fn cmd_run(
    da: &mut DAXFlash,
    args: &[String],
    app_config: &AppConfig,
    log_level: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err("用法: mtkclient run <script.json>".into());
    }

    let script_path = &args[0];
    let script = parse_script(script_path)?;

    // Show script information
    if !script.name.is_empty() {
        info!("{}", format!("═══ 脚本: {} ═══", script.name).cyan().bold());
    }
    if !script.description.is_empty() {
        info!("  {}", script.description);
    }
    info!(
        "{}",
        format!(
            "共 {} 条命令, continue_on_error={}",
            script.commands.len(),
            script.continue_on_error
        )
        .dimmed()
    );
    println!();

    let mut success_count = 0;
    let mut fail_count = 0;
    let mut failed_commands = Vec::new();

    for (i, cmd_str) in script.commands.iter().enumerate() {
        if crate::cancel::requested() || crate::cancel::force_requested() {
            error!("{}", "用户取消，脚本中断".red().bold());
            break;
        }

        info!(
            "{}",
            format!("[{}/{}] {}", i + 1, script.commands.len(), cmd_str.cyan())
        );

        // Parse the command
        let parts: Vec<String> = cmd_str.split_whitespace().map(String::from).collect();
        if parts.is_empty() {
            continue;
        }

        let sub_cmd = parts[0].as_str();
        let sub_args = &parts[1..];

        let result: Result<(), Box<dyn std::error::Error>> = super::dispatch_cmd(
            da,
            sub_cmd,
            sub_args,
            app_config.verify,
            log_level,
            app_config,
        )
        .map_err(|e| e.into());

        match result {
            Ok(()) => {
                success_count += 1;
                info!(
                    "{}",
                    format!("[{}/{}] ✓ 完成", i + 1, script.commands.len()).green()
                );
            }
            Err(e) => {
                fail_count += 1;
                failed_commands.push((cmd_str.clone(), e.to_string()));
                error!(
                    "{}",
                    format!("[{}/{}] ✗ 失败: {}", i + 1, script.commands.len(), e).red()
                );
                if !script.continue_on_error {
                    error!("{}", "脚本中止（continue_on_error=false）".red().bold());
                    break;
                }
            }
        }
        println!();
    }

    // Print the execution summary
    info!(
        "{}",
        format!(
            "═══ 执行完毕: {} 成功, {} 失败, 共 {} 条命令 ═══",
            success_count,
            fail_count,
            script.commands.len()
        )
        .cyan()
        .bold()
    );

    if !failed_commands.is_empty() {
        error!("{}", "失败的命令:".red().bold());
        for (cmd, reason) in &failed_commands {
            error!("  {} → {}", cmd.red(), reason.dimmed());
        }
    }

    if fail_count > 0 {
        Err(format!("{} 条命令执行失败", fail_count).into())
    } else {
        Ok(())
    }
}
