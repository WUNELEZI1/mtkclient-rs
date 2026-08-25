//! 内存读取变体（readflash_data_ex，全量读入内存）
//!
//! 见 [`crate::da::xflash::io`] 模块文档。

use log::{trace, warn};

use crate::da::xflash::DAXFlash;
use crate::da::xflash::protocol::{CMD_MAGIC, GET_PKT_LEN, SET_PKT_LEN};

use super::{READFLASH_READ_TIMEOUT_MS, parse_packet_length, with_read_timeout};

impl<'a> DAXFlash<'a> {
    /// 读取 flash 数据（全量到内存），用于小分区或需要内存操作的场景
    pub(crate) fn readflash_data_ex(
        &mut self,
        addr: u64,
        size: u64,
        parttype: u32,
    ) -> Result<Vec<u8>, String> {
        // 防御：禁止把超大分区整块读入内存（避免 OOM 崩溃）。
        // 大分区请走流式读取（readflash_to_file / r <分区> <文件> / rl <目录>）。
        const MAX_IN_MEMORY_READ: u64 = 256 * 1024 * 1024; // 256 MiB
        if size > MAX_IN_MEMORY_READ {
            return Err(format!(
                "分区过大 ({:.2} GiB)，无法整块读入内存。请使用流式读取：r <分区> <文件> 或 rl <目录>",
                size as f64 / 1024.0 / 1024.0 / 1024.0
            ));
        }

        // 1. get_packet_length — send_devctrl 内部已包含完整的 xread + status 握手
        //    注意：部分设备/DA（尤其 Preloader 模式会话复用）对 GET_PKT_LEN 返回异常
        //    status，但包长度仅用于优化分块，读取循环按设备实际返回的分包处理，
        //    故失败时不中断（对齐 readflash_to_file 的容错行为），避免 printgpt 等命令无谓失败。
        //    性能关键：主动 SET_PKT_LEN 抬升到 2MB（0x200000）批量传输，对齐刷机匣。
        let reported = match self.send_devctrl(GET_PKT_LEN, None) {
            Ok(data) => parse_packet_length(&data),
            Err(e) => {
                warn!(
                    "readflash_data_ex: get_packet_length 获取失败（已忽略，继续读取）: {}",
                    e
                );
                None
            }
        };
        let target = match reported {
            Some(v) if v >= 0x200000 => v.min(0x400000),
            _ => 0x200000,
        };
        // 主动 SET_PKT_LEN 抬升。SET 成功后重新 GET 确认 DA 实际接受的包长度，
        // 防 DA 端溢出：部分 DA 只接受更小的值，盲目按请求值传输会触发溢出/截断。
        // 仅当 DA 明确回报更小正数时才回退，GET 失败/返回 0 沿用请求值。
        if self
            .send_devctrl(SET_PKT_LEN, Some(&(target as u32).to_le_bytes()))
            .is_ok()
        {
            if let Ok(ack) = self.send_devctrl(GET_PKT_LEN, None) {
                if let Some(actual) = parse_packet_length(&ack) {
                    if actual > 0 && actual < target {
                        warn!(
                            "DA 实际接受读包长度 {} < 请求 {}，回退以避免 DA 端溢出",
                            actual, target
                        );
                    }
                }
            }
        } else {
            trace!("SET_PKT_LEN 不受支持（已忽略）");
        }

        // 2. 发送 READ_DATA 命令及参数
        self.send_read_data_cmd(addr, size, parttype)?;

        // 3. 数据读取循环（全量到内存）
        // 用较长读超时包裹整个读取逻辑（串口默认仅 1s，流式读取会超时失败）。
        with_read_timeout(self, READFLASH_READ_TIMEOUT_MS, |da| {
            let mut buffer = Vec::with_capacity(size as usize);
            let mut remaining = size as usize;

            while remaining > 0 {
                let mut hdr = [0u8; 12];
                match da.preloader.device.read_exact(&mut hdr) {
                    Ok(0) => {
                        trace!("[readflash_data] ZLP on header read, ending loop");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        trace!("[readflash_data] read header error: {}", e);
                        break;
                    }
                }

                let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let slength = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);

                if magic != CMD_MAGIC {
                    trace!(
                        "[readflash_data] bad magic: 0x{:08X} at offset {}, ending loop",
                        magic,
                        buffer.len()
                    );
                    break;
                }

                if slength == 4 {
                    // 4 字节小包/心跳：栈上临时缓冲，避免逐包堆分配；全零心跳直接跳过不计入 buffer
                    let mut tiny = [0u8; 4];
                    if let Err(e) = da.preloader.device.read_exact(&mut tiny) {
                        trace!("[readflash_data] read data(small) error: {}", e);
                        break;
                    }
                    if tiny.iter().all(|&b| b == 0) {
                        trace!("[readflash_data] 心跳包，跳过");
                        continue;
                    }
                    buffer.extend_from_slice(&tiny);
                    remaining = remaining.saturating_sub(4);
                } else {
                    // 正常数据：直接读入 buffer 尾部增长区，消除逐包 vec! 堆分配
                    let slen = slength as usize;
                    // DA 可能在数据块末尾附带尾帧（实测 MT6768 DA 在 chunk 长度里多报若干字节，
                    // 如 8 字节状态/校验）。只保留请求所需字节(remaining)，多余的尾帧读完丢弃，
                    // 保持流对齐以便后续读取独立末态包。mtkclient 不校验 buffer 大小故可容忍，
                    // 这里精确截取，避免把尾帧混入返回数据影响上层 GPT/分区解析。
                    let take = slen.min(remaining);
                    let discard = slen - take;
                    let old_len = buffer.len();
                    buffer.resize(old_len + take, 0);
                    if take > 0
                        && let Err(e) = da.preloader.device.read_exact(&mut buffer[old_len..])
                    {
                        trace!("[readflash_data] read data error: {}", e);
                        buffer.truncate(old_len);
                        break;
                    }
                    if discard > 0 {
                        let mut junk = vec![0u8; discard];
                        if let Err(e) = da.preloader.device.read_exact(&mut junk) {
                            trace!("[readflash_data] drain trailer error: {}", e);
                            break;
                        }
                    }
                    remaining = remaining.saturating_sub(take);
                }

                if let Err(e) = da.ack_silent() {
                    trace!("[readflash_data] send_ack failed: {}", e);
                    break;
                }
                if remaining == 0 {
                    trace!("[readflash_data] 最后一包 ACK 已发送，等待最终状态");
                    break;
                }
            }

            // 完整性校验：① 设备可能提前结束（ZLP / 读错误 / 坏 magic / ACK 失败）；
            // ② 心跳包(slength==4 全零)与零长包被 continue 跳过，不计入 buffer。
            // 若实际收到的真实数据字节数 < size，静默返回会让上层（GPT 解析、
            // preloader 提取、分区校验等）拿到不完整的“假成功”结果，故显式报错。
            // 若 > size（DA 尾帧未被完全丢弃的极端情况）则截断到 size，避免脏数据。
            if buffer.len() < size as usize {
                return Err(format!(
                    "读取数据不完整: 期望 {} 字节，实际仅收到 {} 字节（设备提前结束数据传输）",
                    size,
                    buffer.len()
                ));
            }
            if buffer.len() > size as usize {
                buffer.truncate(size as usize);
            }

            da.readflash_final_status()?;
            trace!("[readflash_data] total read {} bytes", buffer.len());
            Ok(buffer)
        })
    }
}
