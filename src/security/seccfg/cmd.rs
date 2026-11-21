//! seccfg 查询命令入口（只读）。
//!
//! 解锁/上锁已统一走能力驱动路径 [`crate::ops::seccfg::SeccfgOp`]（V3/V4 全版本支持），
//! 本模块只保留 `zyb seccfg info` 的在线/离线分析。

use log::info;

use crate::da::DAXFlash;

/// 在线读取 seccfg 并输出结构化分析（`zyb seccfg info`）
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

/// 离线分析 seccfg 镜像文件（`zyb seccfg info <file>`）
pub fn seccfg_info_file(path: &str) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {}", path, e))?;
    info!("离线分析 seccfg: {} ({} 字节)", path, data.len());
    super::info::print_seccfg_report(&data)
}
