//! DA 设备 IO 与控制命令
//!
//! 包含 DAXFlash 的设备读写、复位、关闭命令：
//! - `readflash_data`：通过 XFlash 协议读取 flash 原始字节
//! - `ack_silent`：发送 ACK 不读响应（避免偷吃下一个包）
//! - `reset_device`：通过 DA 重启设备（CMD_RESET）
//! - `close_device`：关闭设备（可选 jump_bl 重启）
//! - `patch_vbmeta`：vbmeta 修补占位
//!
//! 拆分动机：这些是"已加载 DA 之后"与设备交互的命令，与 DA 加载主流程
//! 完全解耦，独立成模块便于审查和单元测试。

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use crate::da::xflash::DAXFlash;
use crate::usb::log::QUIET_USB_READ;

pub(crate) mod read;
pub(crate) mod read_ex;
pub(crate) mod write;

struct UsbReadQuietGuard(bool);

impl Drop for UsbReadQuietGuard {
    fn drop(&mut self) {
        QUIET_USB_READ.store(self.0, Ordering::Relaxed);
    }
}

/// 串口读超时 RAII guard。
///
/// 串口 crate 的 `read_exact` 语义是"单次 `read` 在超时内填满整个 buffer"，
/// 而本工具串口默认超时仅 `SERIAL_OPEN_TIMEOUT_MS`(1s)。readflash 读取 GPT/分区
/// 是持续数秒的流式传输，数据流中途任何超过 1s 的包间隔（设备处理/ACK 等待）
/// 都会让 `read_exact` 报 `Operation timed out` —— 典型表现即
/// "readflash final status header: serial read_exact: Operation timed out"。
/// 故在 readflash 数据阶段用较长读超时覆盖整包最大间隔。USB 路径的 read_exact
/// 为自建循环，长超时同样安全。
///
/// （实现见下方 `with_read_timeout`：RAII 风格包裹整个读取逻辑，自动恢复原超时。）
const READFLASH_READ_TIMEOUT_MS: u64 = 10_000;

/// DA 对每个读数据块在末尾附加的尾帧长度（字节）。
///
/// 实测 MT6768 / MT6771 的 DA（含 Preloader 与 BROM 模式）在 XFlash 读响应中，
/// 每个数据块 = `12 字节头(magic/type/len)` + `slen` 字节，其中 `slen = 真实数据 + 尾帧`。
/// 尾帧为 8 字节状态/校验。**无论该块是否为最后一块，尾帧都必须剥离**——否则中间块的
/// 尾帧会被当作数据写入 buffer，导致整段数据按 8 字节/块错位，表现为 GPT 分区条目 CRC
/// 失败、printgpt 读空（commit 03071dd 仅修正了最后一块，多块传输仍错位）。
/// 见 `read_ex.rs` / `read.rs` 数据循环。
pub(crate) const DA_READ_PER_CHUNK_TRAILER: usize = 8;

/// 临时设置较长读超时执行回调，完成后恢复原超时（RAII，覆盖所有提前 return）。
/// 用于 readflash 数据读取阶段：串口默认超时仅 1s，流式读取持续数秒会超时失败。
fn with_read_timeout<T>(
    da: &mut DAXFlash,
    ms: u64,
    f: impl FnOnce(&mut DAXFlash) -> Result<T, String>,
) -> Result<T, String> {
    let orig = da.preloader.device.get_timeout();
    da.preloader.device.set_timeout(Duration::from_millis(ms));
    let result = f(da);
    da.preloader.device.set_timeout(orig);
    result
}

fn quiet_usb_reads_temporarily() -> UsbReadQuietGuard {
    UsbReadQuietGuard(QUIET_USB_READ.swap(true, Ordering::Relaxed))
}

fn ensure_output_file_path(path: &str) -> Result<(), String> {
    let path_ref = std::path::Path::new(path);
    if path_ref.is_dir() {
        return Err(format!("输出路径是目录，不是文件: {}", path));
    }
    if let Some(parent) = path_ref.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建输出目录失败 '{}': {}", parent.display(), e))?;
    }
    Ok(())
}

fn parse_packet_length(data: &[u8]) -> Option<usize> {
    if data.len() >= 4 {
        let value = u32::from_le_bytes(data[..4].try_into().ok()?);
        if value > 0 {
            return Some(value as usize);
        }
    }
    None
}

fn resume_path_for(output_file: &str) -> String {
    crate::resume::read_resume_path(output_file)
}

fn write_resume_file(
    output_file: &str,
    addr: u64,
    size: u64,
    parttype: u32,
    written: u64,
    active_read: bool,
    packet_len: Option<usize>,
) -> Result<(), String> {
    let packet = packet_len.unwrap_or(0);
    let content = format!(
        "output={}\naddr=0x{:X}\nsize={}\nparttype={}\nwritten={}\nactive_read={}\npacket_len={}\n",
        output_file, addr, size, parttype, written, active_read, packet
    );
    std::fs::write(resume_path_for(output_file), content)
        .map_err(|e| format!("写入续传状态失败: {}", e))
}

fn remove_resume_file(output_file: &str) {
    let _ = std::fs::remove_file(resume_path_for(output_file));
}

fn active_resume_matches(output_file: &str, start_offset: u64) -> bool {
    // 检查输出文件是否存在
    let file_size = match std::fs::metadata(output_file) {
        Ok(m) => m.len(),
        Err(_) => return false,
    };

    let Ok(content) = std::fs::read_to_string(resume_path_for(output_file)) else {
        return false;
    };

    let has_active_read = crate::resume::is_active_read(&content);
    let resume_written = crate::resume::parse_u64_field(&content, "written=");

    match resume_written {
        Some(written) => {
            // 允许 512 字节容差：实际文件大小和 resume 记录可能因对齐有微小差异
            let size_match = file_size.abs_diff(written) <= 512;
            let offset_match = start_offset.abs_diff(written) <= 512;
            has_active_read && size_match && offset_match
        }
        None => false,
    }
}

fn spawn_dump_writer(
    output_file: &str,
    addr: u64,
    size: u64,
    parttype: u32,
    start_offset: u64,
    packet_len: Option<usize>,
) -> Result<
    (
        SyncSender<Option<Vec<u8>>>,
        Receiver<Vec<u8>>,
        std::thread::JoinHandle<Result<u64, String>>,
    ),
    String,
> {
    const CHANNEL_CAP: usize = 128;
    const FLUSH_INTERVAL: u64 = 128 * 1024 * 1024;
    const RESUME_INTERVAL: u64 = 64 * 1024 * 1024;
    const BUF_WRITER_CAP: usize = 64 * 1024 * 1024;

    let output_path = output_file.to_string();
    let (tx, rx): (SyncSender<Option<Vec<u8>>>, Receiver<Option<Vec<u8>>>) =
        mpsc::sync_channel(CHANNEL_CAP);
    let (recycle_tx, recycle_rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) =
        mpsc::sync_channel(CHANNEL_CAP);

    let handle = std::thread::Builder::new()
        .name("flash_dump_writer".to_string())
        .spawn(move || -> Result<u64, String> {
            use std::io::{BufWriter, Write};

            let raw_file = if start_offset > 0 {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&output_path)
                    .map_err(|e| format!("打开文件失败 '{}': {}", output_path, e))?
            } else {
                std::fs::File::create(&output_path)
                    .map_err(|e| format!("创建文件失败 '{}': {}", output_path, e))?
            };
            let mut file = BufWriter::with_capacity(BUF_WRITER_CAP, raw_file);
            let mut written = start_offset;
            let mut last_flush_pos = start_offset;
            let mut last_resume_pos = start_offset;

            while let Ok(message) = rx.recv() {
                let Some(data) = message else {
                    break;
                };
                file.write_all(&data)
                    .map_err(|e| format!("写入文件失败: {}", e))?;
                written += data.len() as u64;
                let mut data = data;
                data.clear();
                let _ = recycle_tx.try_send(data); // 非阻塞：fast 路径主循环不消费 recycle 池，阻塞会死锁

                if written - last_flush_pos >= FLUSH_INTERVAL || written >= size {
                    file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
                    last_flush_pos = written;
                }
                if written - last_resume_pos >= RESUME_INTERVAL || written >= size {
                    write_resume_file(
                        &output_path,
                        addr,
                        size,
                        parttype,
                        written,
                        true,
                        packet_len,
                    )?;
                    last_resume_pos = written;
                }
            }

            file.flush().map_err(|e| format!("flush 文件失败: {}", e))?;
            write_resume_file(
                &output_path,
                addr,
                size,
                parttype,
                written,
                true,
                packet_len,
            )?;
            Ok(written)
        })
        .map_err(|e| format!("创建写入线程失败: {}", e))?;

    Ok((tx, recycle_rx, handle))
}

fn finish_dump_writer(
    tx: SyncSender<Option<Vec<u8>>>,
    handle: std::thread::JoinHandle<Result<u64, String>>,
) -> Result<u64, String> {
    let _ = tx.send(None);
    handle.join().map_err(|_| "写入线程 panic".to_string())?
}

fn acquire_dump_buffer(recycle_rx: &Receiver<Vec<u8>>, len: usize) -> Vec<u8> {
    let mut buf = recycle_rx
        .try_recv()
        .unwrap_or_else(|_| Vec::with_capacity(len));
    if buf.capacity() < len {
        buf.reserve(len - buf.capacity());
    }
    buf.resize(len, 0);
    buf
}

fn read_header_with_optional_queue(
    device: &mut dyn crate::preloader::transport::BromTransport,
    hdr: &mut [u8; 12],
    queued_header: &mut bool,
) -> Result<(), String> {
    if *queued_header {
        let got = device.complete_read_request(hdr)?;
        *queued_header = false;
        if got < hdr.len() {
            device.read_exact(&mut hdr[got..])?;
        }
    } else {
        device.read_exact(hdr)?;
    }
    Ok(())
}

fn final_read_status_from_payload(payload: &[u8]) -> Result<(), String> {
    if payload.len() == 4 {
        let status = u32::from_le_bytes(payload.try_into().unwrap());
        if status != 0 {
            return Err(format!(
                "Read completed with error status: 0x{:08X}",
                status
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_rejects_directory() {
        let dir = std::env::temp_dir();
        let err = ensure_output_file_path(dir.to_str().unwrap()).unwrap_err();

        assert!(err.contains("输出路径是目录"));
    }

    #[test]
    fn quiet_usb_guard_restores_previous_state() {
        QUIET_USB_READ.store(false, Ordering::Relaxed);
        {
            let _guard = quiet_usb_reads_temporarily();
            assert!(QUIET_USB_READ.load(Ordering::Relaxed));
        }

        assert!(!QUIET_USB_READ.load(Ordering::Relaxed));
    }

    #[test]
    fn parse_packet_length_reads_little_endian_u32() {
        assert_eq!(
            parse_packet_length(&0x40000u32.to_le_bytes()),
            Some(0x40000)
        );
        assert_eq!(parse_packet_length(&0u32.to_le_bytes()), None);
        assert_eq!(parse_packet_length(&[1, 2, 3]), None);
    }

    #[test]
    fn active_resume_matches_only_when_written_offset_matches() {
        let output =
            std::env::temp_dir().join(format!("resume_match_{}_{}.img", std::process::id(), "a"));
        let output = output.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);

        // 创建与 written=0x2000 匹配的输出文件（active_resume_matches 需要文件存在且大小匹配）
        std::fs::write(&output, vec![0u8; 0x2000]).unwrap();
        write_resume_file(&output, 0x1000, 0x4000, 8, 0x2000, true, Some(0x1000)).unwrap();

        assert!(active_resume_matches(&output, 0x2000));
        assert!(!active_resume_matches(&output, 0x1000));

        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);
    }

    #[test]
    fn dump_writer_writes_data_and_resume_metadata() {
        let output = std::env::temp_dir().join(format!("dump_writer_{}.img", std::process::id()));
        let output = output.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);

        let (tx, _recycle_rx, handle) =
            spawn_dump_writer(&output, 0x1000, 6, 8, 0, Some(2)).unwrap();
        tx.send(Some(vec![1, 2, 3])).unwrap();
        tx.send(Some(vec![4, 5, 6])).unwrap();
        let written = finish_dump_writer(tx, handle).unwrap();

        assert_eq!(written, 6);
        assert_eq!(std::fs::read(&output).unwrap(), vec![1, 2, 3, 4, 5, 6]);
        let resume = std::fs::read_to_string(resume_path_for(&output)).unwrap();
        assert!(resume.contains("written=6"));
        assert!(resume.contains("packet_len=2"));

        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);
    }

    #[test]
    fn final_read_status_accepts_zero_and_rejects_error() {
        assert!(final_read_status_from_payload(&0u32.to_le_bytes()).is_ok());
        let err = final_read_status_from_payload(&1u32.to_le_bytes()).unwrap_err();
        assert!(err.contains("0x00000001"));
        assert!(final_read_status_from_payload(&[]).is_ok());
    }
}
