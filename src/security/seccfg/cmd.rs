//! seccfg query command entry (read-only).
//!
//! Unlock/lock now go through the unified capability-driven path [`crate::ops::seccfg::SeccfgOp`] (all V3/V4 versions),
//! so this module only keeps online/offline analysis for `zyb seccfg info`.

use log::info;

use crate::da::DAXFlash;

/// Read seccfg online and print a structured report (`zyb seccfg info`).
pub fn seccfg_info(da: &mut DAXFlash) -> Result<(), String> {
    let (seccfg_addr, seccfg_size) = da.find_partition_addr("seccfg")?;
    let data = da.readflash_data(seccfg_addr, seccfg_size)?;
    info!(
        "读取 seccfg 分区: addr=0x{:X}, size={} 字节",
        seccfg_addr,
        data.len()
    );
    super::info::print_seccfg_report(&data)
}

/// Analyze a seccfg image file offline (`zyb seccfg info <file>`).
pub fn seccfg_info_file(path: &str) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {}", path, e))?;
    info!("离线分析 seccfg: {} ({} 字节)", path, data.len());
    super::info::print_seccfg_report(&data)
}
