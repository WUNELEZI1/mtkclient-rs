//! Gzip / tar.gz 压缩/解压工具模块
//!
//! 将 payload 和 DA 文件以 .tar.gz 形式打包存储，程序启动时后台线程预加载到内存。
//! 消除 per-file gzip 头开销，减少文件数量，提升启动后文件读取速度。

use flate2::read::GzDecoder;
use log::{debug, trace};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// 全局 tar.gz 缓存（线程安全，OnceLock 保证只初始化一次）
static TAR_CACHE: OnceLock<Arc<TarArchiveCache>> = OnceLock::new();

/// 标记系统中是否存在 tar.gz 文件（程序启动时设置）
/// 用于 read_file_auto_decompress 判断是否需要等待后台加载
static TAR_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Tar.gz 内存缓存：所有文件解压后存入 HashMap，key 为 tar 内路径
pub struct TarArchiveCache {
    files: HashMap<String, Vec<u8>>,
}

impl TarArchiveCache {
    /// 从 .tar.gz 文件加载所有内容到内存
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        trace!("开始加载 tar.gz: {}", path.display());
        let file = File::open(path)
            .map_err(|e| format!("打开 tar.gz 失败 {}: {}", path.display(), e))?;
        let decoder = GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut files = HashMap::new();
        let mut total_bytes = 0usize;

        for entry in archive
            .entries()
            .map_err(|e| format!("读取 tar 条目失败: {}", e))?
        {
            let mut entry = entry.map_err(|e| format!("tar 条目错误: {}", e))?;
            let path_bytes = entry
                .path()
                .map_err(|e| format!("tar 路径错误: {}", e))?;
            let name = path_bytes.to_string_lossy().replace('\\', "/");

            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|e| format!("读取 tar 条目数据失败: {}", e))?;

            total_bytes += data.len();
            files.insert(name, data);
        }

        debug!(
            "tar.gz 加载完成: {} 个文件, 共 {} 字节 ({} MiB)",
            files.len(),
            total_bytes,
            total_bytes / 1024 / 1024
        );
        Ok(Self { files })
    }

    /// 按路径查找文件内容（支持多种路径格式）
    pub fn get(&self, path: &str) -> Option<&[u8]> {
        // 1. 精确匹配
        let normalized = path.replace('\\', "/");
        if let Some(data) = self.files.get(&normalized) {
            return Some(data.as_slice());
        }

        // 2. 仅匹配文件名（如 mt6768_payload.bin）
        if let Some(file_name) = Path::new(path).file_name() {
            let name = file_name.to_string_lossy();
            if let Some(data) = self.files.get(name.as_ref()) {
                return Some(data.as_slice());
            }
            // 3. 尝试带前缀匹配（如 payload/mt6768_payload.bin）
            for prefix in &["payload/", "bin/"] {
                let prefixed = format!("{}{}", prefix, name);
                if let Some(data) = self.files.get(&prefixed) {
                    return Some(data.as_slice());
                }
            }
        }

        None
    }

    /// 获取缓存文件数量
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// 初始化全局 tar.gz 缓存（在后台线程中执行，不阻塞主线程）
pub fn init_tar_cache_async(tar_path: PathBuf) {
    TAR_AVAILABLE.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        debug!("后台线程: 开始加载 tar.gz...");
        match TarArchiveCache::load_from_file(&tar_path) {
            Ok(cache) => {
                let _ = TAR_CACHE.set(Arc::new(cache));
                debug!("后台线程: tar.gz 加载完成");
            }
            Err(e) => {
                debug!("后台线程: tar.gz 加载失败: {}", e);
            }
        }
    });
}

/// 尝试从 tar 缓存读取文件（如果已加载）
pub fn read_from_tar(path: &Path) -> Option<Vec<u8>> {
    let cache = TAR_CACHE.get()?;
    let key = path.to_string_lossy();
    cache.get(&key).map(|data| data.to_vec())
}

/// 读取文件，支持多种来源（按优先级）：
/// 1. 原始文件（如果存在，最快路径）
/// 2. tar.gz 内存缓存（后台线程预加载，必要时同步等待）
/// 3. .bin.gz 单文件压缩（兼容旧格式）
pub fn read_file_auto_decompress(path: &Path) -> Result<Vec<u8>, String> {
    // 1. 原始文件直接读取
    if path.exists() {
        debug!("读取原始文件: {}", path.display());
        return std::fs::read(path)
            .map_err(|e| format!("读取文件失败 {}: {}", path.display(), e));
    }

    // 2. tar.gz 缓存
    // 如果 tar.gz 存在但后台线程还没加载完，同步等待（最多 5 秒）
    if TAR_AVAILABLE.load(Ordering::SeqCst) {
        if TAR_CACHE.get().is_none() {
            debug!("等待 tar.gz 后台加载完成...");
            for _ in 0..500 {
                if TAR_CACHE.get().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if let Some(data) = read_from_tar(path) {
            debug!("从 tar.gz 缓存读取: {}", path.display());
            return Ok(data);
        }
    }

    // 3. 尝试读取 .gz 单文件压缩版本（兼容旧格式）
    let gz_path = if let Some(ext) = path.extension() {
        if ext == "bin" {
            path.with_extension("bin.gz")
        } else {
            let mut gz_name = path.file_name().unwrap_or_default().to_os_string();
            gz_name.push(".gz");
            path.with_file_name(gz_name)
        }
    } else {
        path.with_extension("gz")
    };

    if gz_path.exists() {
        debug!("解压读取单文件: {} → {}", gz_path.display(), path.display());
        let file = File::open(&gz_path)
            .map_err(|e| format!("打开压缩文件失败 {}: {}", gz_path.display(), e))?;
        let mut decoder = GzDecoder::new(file);
        let mut data = Vec::new();
        decoder
            .read_to_end(&mut data)
            .map_err(|e| format!("解压失败 {}: {}", gz_path.display(), e))?;
        return Ok(data);
    }

    Err(format!(
        "找不到文件: {}（也未找到 tar.gz 缓存或压缩版本）",
        path.display()
    ))
}

/// 压缩单个文件（gzip -9 级别）
pub fn compress_file(src: &Path, dst: &Path) -> Result<(), String> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    let data = std::fs::read(src)
        .map_err(|e| format!("读取源文件失败 {}: {}", src.display(), e))?;

    let dst_file = File::create(dst)
        .map_err(|e| format!("创建压缩文件失败 {}: {}", dst.display(), e))?;
    let mut encoder = GzEncoder::new(dst_file, Compression::best());
    encoder
        .write_all(&data)
        .map_err(|e| format!("写入压缩数据失败: {}", e))?;
    encoder
        .finish()
        .map_err(|e| format!("完成压缩失败: {}", e))?;

    let original_size = data.len();
    let compressed_size = std::fs::metadata(dst)
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    let ratio = if original_size > 0 {
        (compressed_size as f64 / original_size as f64) * 100.0
    } else {
        0.0
    };

    debug!(
        "压缩完成: {} → {} ({} → {} 字节, {:.1}%)",
        src.display(),
        dst.display(),
        original_size,
        compressed_size,
        ratio
    );
    Ok(())
}

/// 批量压缩目录下的所有 .bin 文件（跳过已存在的 .bin.gz）
pub fn compress_dir_bins(dir: &Path) -> Result<(usize, usize), String> {
    if !dir.is_dir() {
        return Err(format!("不是目录: {}", dir.display()));
    }

    let mut compressed = 0usize;
    let mut skipped = 0usize;

    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("读取目录失败 {}: {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let path = entry.path();

        if let Some(ext) = path.extension() {
            if ext == "bin" {
                let gz_path = path.with_extension("bin.gz");
                if gz_path.exists() {
                    skipped += 1;
                    continue;
                }
                compress_file(&path, &gz_path)?;
                compressed += 1;
            }
        }
    }

    Ok((compressed, skipped))
}

/// 批量删除原始 .bin 文件（保留 .bin.gz）
pub fn remove_raw_bins(dir: &Path) -> Result<usize, String> {
    if !dir.is_dir() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(dir)
        .map_err(|e| format!("读取目录失败 {}: {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let path = entry.path();

        if let Some(ext) = path.extension() {
            if ext == "bin" {
                let gz_path = path.with_extension("bin.gz");
                if gz_path.exists() {
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("删除文件失败 {}: {}", path.display(), e))?;
                    removed += 1;
                }
            }
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试：TarArchiveCache::get() 路径匹配
    #[test]
    fn test_tar_cache_get() {
        let cache = TarArchiveCache {
            files: [
                ("payload/mt6768_payload.bin".into(), b"payload_data".to_vec()),
                ("bin/MTK_DA_V5.bin".into(), b"da_data".to_vec()),
            ]
            .into(),
        };

        // 精确匹配
        assert_eq!(cache.get("payload/mt6768_payload.bin"), Some(b"payload_data".as_slice()));
        assert_eq!(cache.get("bin/MTK_DA_V5.bin"), Some(b"da_data".as_slice()));

        // 纯文件名匹配
        assert_eq!(cache.get("mt6768_payload.bin"), Some(b"payload_data".as_slice()));
        assert_eq!(cache.get("MTK_DA_V5.bin"), Some(b"da_data".as_slice()));

        // 不存在的文件
        assert!(cache.get("not_exist.bin").is_none());
    }

    /// 测试：从实际 mtkclient_data.tar.gz 加载并读取文件
    #[test]
    fn test_load_real_tar_gz() {
        let tar_path = PathBuf::from(r"D:\test\ZybClient\target\debug\mtkclient_data.tar.gz");
        if !tar_path.exists() {
            eprintln!("跳过: mtkclient_data.tar.gz 不存在");
            return;
        }

        let cache = TarArchiveCache::load_from_file(&tar_path).unwrap();
        assert_eq!(cache.len(), 64, "应有 64 个文件");

        // 验证几个关键文件
        let payload_file = cache.get("payload/mt6768_payload.bin");
        assert!(payload_file.is_some(), "应能读取 mt6768_payload.bin");
        assert!(!payload_file.unwrap().is_empty(), "mt6768_payload.bin 不应为空");

        let da_file = cache.get("bin/MTK_DA_V5.bin");
        assert!(da_file.is_some(), "应能读取 MTK_DA_V5.bin");
        let da_data = da_file.unwrap();
        assert!(da_data.len() > 1_000_000, "DA V5 应 > 1MB，实际 {} bytes", da_data.len());

        // 测试 find_tar_gz_path
        assert_eq!(find_tar_gz_path(), Some(tar_path.clone()));
    }

}

/// 查找 tar.gz 文件路径（搜索多个常见位置）
pub fn find_tar_gz_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // 1. exe_dir/mtkclient_data.tar.gz
            let direct = exe_dir.join("mtkclient_data.tar.gz");
            if direct.exists() {
                return Some(direct);
            }
            // 2. exe_dir 的父目录（测试/集成模式下 exe 可能在 deps/ 子目录中）
            if let Some(parent) = exe_dir.parent() {
                let parent_path = parent.join("mtkclient_data.tar.gz");
                if parent_path.exists() {
                    return Some(parent_path);
                }
            }
            // 3. exe_dir/payload/mtkclient_data.tar.gz
            let in_payload = exe_dir.join("payload").join("mtkclient_data.tar.gz");
            if in_payload.exists() {
                return Some(in_payload);
            }
            // 4. 开发模式：项目根目录
            if let Some(project_root) = exe_dir.parent().and_then(|p| p.parent()) {
                let dev_path = project_root.join("mtkclient_data.tar.gz");
                if dev_path.exists() {
                    return Some(dev_path);
                }
            }
        }
    }
    None
}
