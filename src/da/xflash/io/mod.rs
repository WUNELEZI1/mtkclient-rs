//! DA device IO and control commands
//!
//! DAXFlash commands for device read/write, reset, and close:
//! - `readflash_data`: read raw flash bytes via the XFlash protocol
//! - `ack_silent`: send ACK without reading a response (avoid consuming the next packet)
//! - `reset_device`: reboot the device via DA (CMD_RESET)
//! - `close_device`: close the device (optionally reboot via jump_bl)
//! - `patch_vbmeta`: vbmeta patch placeholder
//!
//! Split rationale: these commands interact with the device after DA is loaded and are fully
//! decoupled from the DA loading main flow; a separate module eases review and unit testing.

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

/// RAII guard for the serial read timeout.
///
/// The `read_exact` semantics of the serial crate require a single `read` to fill the entire buffer
/// within the timeout, while the default serial timeout of this tool is only `SERIAL_OPEN_TIMEOUT_MS` (1s).
/// Reading GPT/partitions via readflash is a stream lasting several seconds; any packet gap over 1s mid-stream
/// (device processing/ACK wait) makes `read_exact` report `Operation timed out` — typically
/// "readflash final status header: serial read_exact: Operation timed out".
/// So a longer read timeout is used during the readflash data phase to cover the max inter-packet gap. The USB
/// path uses a self-built read_exact loop, where a long timeout is likewise safe.
///
/// (Implementation: `with_read_timeout` below; an RAII-style wrapper around the whole read logic that restores the original timeout.)
const READFLASH_READ_TIMEOUT_MS: u64 = 10_000;

/// Length in bytes of the trailer DA appends to the end of each read data chunk.
///
/// Measured on MT6768 / MT6771 DA (Preloader and BROM modes) in XFlash read responses:
/// each chunk = `12-byte header (magic/type/len)` + `slen` bytes, where `slen = real data + trailer`.
/// The trailer is 8 bytes of status/checksum. **The trailer must be stripped from every chunk, last or not** —
/// otherwise middle-chunk trailers get written as data, shifting the stream by 8 bytes per chunk, which shows up as
/// GPT entry CRC failures and empty printgpt output (commit 03071dd only fixed the last chunk; multi-chunk transfers were still misaligned).
/// See the data loops in `read_ex.rs` / `read.rs`.
pub(crate) const DA_READ_PER_CHUNK_TRAILER: usize = 8;

/// Temporarily set a longer read timeout to run a callback, then restore it (RAII covers all early returns).
/// Used in the readflash data phase: the default serial timeout is only 1s, so a multi-second streaming read would time out.
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
    if data.len() >= 8 {
        let value = u32::from_le_bytes(data[4..8].try_into().ok()?);
        if value > 0 {
            return Some(value as usize);
        }
    }
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
    // Check that the output file exists
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
            // Allow 512-byte tolerance: the file size and resume record may differ slightly due to alignment
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
                let _ = recycle_tx.try_send(data); // non-blocking: the fast-path main loop never drains the recycle pool, so blocking would deadlock

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
        let mut pair = Vec::new();
        pair.extend_from_slice(&0x40000u32.to_le_bytes());
        pair.extend_from_slice(&0x200000u32.to_le_bytes());
        assert_eq!(parse_packet_length(&pair), Some(0x200000));

        let mut pair_with_zero_read = Vec::new();
        pair_with_zero_read.extend_from_slice(&0x40000u32.to_le_bytes());
        pair_with_zero_read.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_packet_length(&pair_with_zero_read), Some(0x40000));

        assert_eq!(parse_packet_length(&0u32.to_le_bytes()), None);
        assert_eq!(parse_packet_length(&[1, 2, 3]), None);
    }

    #[test]
    fn set_packet_length_16_byte_response_is_not_a_length() {
        // 0x040008 = GET_RANDOM_ID: DA returns 16 random bytes (on real-device printgpt logs it
        // was once mistaken for a "SET_PKT_LEN flow payload"). It is never a packet length and must not be parsed as one.
        let mut response = Vec::new();
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());

        assert_eq!(parse_packet_length(&response), None);
    }

    #[test]
    fn get_packet_length_uses_da_reported_read_length() {
        // Real-device GET_PKT_LEN (0x040007) response: [write packet length=2MiB][read packet length=64KiB].
        // The read path must take the second u32 (read packet length), not mistake the write length for the read length.
        // The read path only GETs, never SETs; 0x040008 (GET_RANDOM_ID) must not be sent as SET_PKT_LEN.
        let response = [0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x01, 0x00];
        assert_eq!(parse_packet_length(&response), Some(0x1_0000));
    }

    #[test]
    fn active_resume_matches_only_when_written_offset_matches() {
        let output =
            std::env::temp_dir().join(format!("resume_match_{}_{}.img", std::process::id(), "a"));
        let output = output.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&output);
        remove_resume_file(&output);

        // Create an output file matching written=0x2000 (active_resume_matches requires the file to exist with a matching size)
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
        let err = final_read_status_from_payload(&0xC0010003u32.to_le_bytes()).unwrap_err();
        assert!(err.contains("0xC0010003"));
        assert!(final_read_status_from_payload(&[]).is_ok());
    }
}
