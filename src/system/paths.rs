//! Path management module
//!
//! Resolves paths relative to the executable
//! Bridging the path differences between development and release builds
//!
//! ## Directory layout
//! ```text
//! exe_dir/
//! ├── mtkclient-rs.exe
//! ├── data/                    ← resource data directory
//! │   ├── sdata.json
//! │   ├── MTK_DA_V5.bin
//! │   ├── mt6768/              ← chip-specific payload
//! │   │   └── mt6768_payload.bin
//! │   ├── mt6771/
//! │   │   └── mt6771_payload.bin
//! │   └── generic/             ← generic payload
//! │       ├── generic_dump_payload.bin
//! │       └── ...
//! └── tmp/                     ← debug/cache output directory
//!     ├── .state
//!     ├── gpt.bin
//!     ├── gpt_full.bin
//!     └── usb_debug.log
//! ```
//!
//! ## --data-dir override mechanism
//! The GUI (zybflashtool) can pass an extracted data directory via `--data-dir`;
//! once set, path resolution looks there first.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Global data-dir override (passed by the GUI via --data-dir)
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set the global data-dir (called right after CLI parsing in main)
pub fn set_data_dir(path: PathBuf) {
    let _ = DATA_DIR.set(path);
}

/// Extract the chip directory name from a payload filename
///
/// e.g. `mt6768_payload.bin` → `Some("mt6768")`
fn extract_chip_name(file_name: &str) -> Option<&str> {
    file_name
        .strip_suffix("_payload.bin")
        .filter(|s| s.starts_with("mt") || s.starts_with("MT"))
}

/// Search for a file under the given base directory (tried in priority order)
///
/// Search order:
/// 1. Direct join under the base directory (base/relative path)
/// 2. Direct join under base/data/
/// 3. base/data/{chip_name}/ chip-specific directory
/// 4. base/data/generic/ generic payload directory
/// 5. base/payload/ and base/bin/ legacy compatibility directories
fn search_in_base_dir(base_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    let path = Path::new(rel_path);

    // 1. Direct join
    let direct = base_dir.join(rel_path);
    if direct.exists() {
        return Some(direct);
    }

    let file_name = path.file_name()?.to_string_lossy();

    // 2. data/ subdirectory (file placed directly)
    let data_direct = base_dir.join("data").join(file_name.as_ref());
    if data_direct.exists() {
        return Some(data_direct);
    }

    // 3. data/{chip_name}/ chip-specific subdirectory
    if let Some(chip_name) = extract_chip_name(&file_name) {
        let chip_path = base_dir
            .join("data")
            .join(chip_name)
            .join(file_name.as_ref());
        if chip_path.exists() {
            return Some(chip_path);
        }
    }

    // 4. data/generic/ generic payload subdirectory
    let generic_path = base_dir
        .join("data")
        .join("generic")
        .join(file_name.as_ref());
    if generic_path.exists() {
        return Some(generic_path);
    }

    // 5. payload/ and bin/ legacy compatibility subdirectories
    let payload_path = base_dir.join("payload").join(file_name.as_ref());
    if payload_path.exists() {
        return Some(payload_path);
    }
    let bin_path = base_dir.join("bin").join(file_name.as_ref());
    if bin_path.exists() {
        return Some(bin_path);
    }

    None
}

/// Get a path relative to the executable
///
/// Search order:
/// 0. **--data-dir override** (extracted directory passed by the GUI, highest priority)
/// 1. Same directory as the executable (exe_dir/relative path)
/// 2. exe_dir/data/ subdirectory (including chip-specific and generic payload lookup)
/// 3. exe_dir/payload/ or exe_dir/bin/ subdirectories (legacy compatibility)
/// 4. Development mode: project root (go up 2 levels target/debug/ → target/ → root)
/// 5. Fallback: join directly under the executable directory
pub fn get_exe_relative_path(rel_path: &str) -> PathBuf {
    // 0. --data-dir override (highest priority)
    if let Some(data_dir) = DATA_DIR.get() {
        if let Some(found) = search_in_base_dir(data_dir, rel_path) {
            return found;
        }
    }

    if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        // 1~3. Search under the executable directory (including data/, payload/, bin/)
        if let Some(found) = search_in_base_dir(exe_dir, rel_path) {
            return found;
        }

        // 4. Development mode: go up 2 levels to the project root
        if let Some(project_root) = exe_dir.parent().and_then(|parent_dir| parent_dir.parent()) {
            let dev_path = project_root.join(rel_path);
            if dev_path.exists() {
                return dev_path;
            }
            // Also try the data/ subdirectory in development mode
            if let Some(found) = search_in_base_dir(project_root, rel_path) {
                return found;
            }
        }

        // 5. Fallback: return the joined path directly (even if it does not exist)
        return exe_dir.join(rel_path);
    }
    // Fallback: use the current working directory
    PathBuf::from(rel_path)
}

/// Get a file path under the tmp directory
///
/// Creates tmp/ automatically (next to the executable).
/// Used for temporary files such as debug output and caches.
///
/// ```text
/// exe_dir/tmp/gpt.bin
/// exe_dir/tmp/.state
/// exe_dir/tmp/usb_debug.log
/// ```
pub fn get_tmp_path(file_name: &str) -> PathBuf {
    let tmp_dir = if let Some(data_dir) = DATA_DIR.get() {
        data_dir.join("tmp")
    } else if let Ok(current_exe) = env::current_exe()
        && let Some(exe_dir) = current_exe.parent()
    {
        exe_dir.join("tmp")
    } else {
        PathBuf::from("tmp")
    };

    // Ensure the tmp directory exists
    if !tmp_dir.exists() {
        let _ = std::fs::create_dir_all(&tmp_dir);
    }

    tmp_dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exe_relative_path() {
        let path = get_exe_relative_path("usb_driver/test.inf");
        assert!(path.ends_with("usb_driver/test.inf"));
    }

    #[test]
    fn test_extract_chip_name() {
        assert_eq!(extract_chip_name("mt6768_payload.bin"), Some("mt6768"));
        assert_eq!(extract_chip_name("mt6771_payload.bin"), Some("mt6771"));
        assert_eq!(extract_chip_name("generic_dump_payload.bin"), None);
        assert_eq!(extract_chip_name("MTK_DA_V5.bin"), None);
    }
}
