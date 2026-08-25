//! `Preloader` 初始化流程的剩余原语
//!
//! 本文件承接 `mod.rs` 中的 `impl Preloader` 第二块，包含：
//! - `get_hw_subcode` 读取 HW Subcode
//! - `flush_input` / `flush_input_quick` / `flush_input_poll` 输入缓冲清理
//! - `rword` 读 16 位字基础工具

use super::{
    FLUSH_INPUT_CHUNK, FLUSH_INPUT_MAX_ITER, FLUSH_INPUT_QUICK_CHUNK, FLUSH_INPUT_QUICK_MAX_ITER,
    FLUSH_INPUT_QUICK_TIMEOUT_MS, FLUSH_INPUT_TIMEOUT_MS, POST_FLUSH_TIMEOUT_MS,
};
use crate::preloader::core::Preloader;
use log::trace;
use std::time::Duration;

impl Preloader {
    /// 读取 HW Subcode
    /// Python: echo(0xDB) → rbyte(2)
    #[allow(dead_code)] // 预留：部分芯片需要通过 hw_subcode 区分变体
    pub fn get_hw_subcode(&mut self) -> Result<u16, String> {
        if !self.sendcmd(0xDB)? {
            return Err("获取 HW subcode 失败: echo 0xDB 不匹配".into());
        }
        let mut buf = [0u8; 2];
        self.device
            .read_exact(&mut buf)
            .map_err(|e| format!("read hw subcode: {}", e))?;
        Ok(u16::from_be_bytes(buf))
    }

    /// 清空输入缓冲（串口模式下丢弃所有待读数据，防止 echo mismatch 后读取错位）
    pub fn flush_input(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH] total discarded {} bytes", total);
        }
    }

    /// 快速 flush：短超时（50ms），用于 read32_brom 前置清理
    /// 比常规 flush 快 4 倍，适用于已确认设备状态正常的场景
    pub(crate) fn flush_input_quick(&mut self) {
        self.device
            .set_timeout(Duration::from_millis(FLUSH_INPUT_QUICK_TIMEOUT_MS));
        let mut trash = [0u8; FLUSH_INPUT_QUICK_CHUNK];
        let mut total = 0;
        for _ in 0..FLUSH_INPUT_QUICK_MAX_ITER {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        if total > 0 {
            trace!("[FLUSH_QUICK] discarded {} bytes", total);
        }
    }

    /// 轮询式 flush：用短超时反复读取，直到 USB 缓冲区为空（设备 ready）
    /// 比固定 sleep 更快 — 设备准备好了就立刻返回
    pub fn flush_input_poll(&mut self, interval: Duration, max_iters: u32) -> Result<(), String> {
        self.device.set_timeout(interval);
        let mut trash = [0u8; FLUSH_INPUT_CHUNK];
        let mut total = 0;
        for i in 0..max_iters {
            match self.device.read(&mut trash) {
                Ok(n) if n > 0 => total += n,
                _ => {
                    // 缓冲区空了，设备 ready
                    break;
                }
            }
            if i == max_iters - 1 {
                trace!("[FLUSH_POLL] 轮询 {} 次, 丢弃 {} bytes", max_iters, total);
            }
        }
        self.device
            .set_timeout(Duration::from_millis(POST_FLUSH_TIMEOUT_MS));
        Ok(())
    }

    /// 读 16 位字（2 字节，big-endian，对齐 Python DeviceHandler.rword(little=False)）
    pub fn rword(&mut self) -> Result<u16, String> {
        let mut buf = [0u8; 2];
        self.device.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }
}
