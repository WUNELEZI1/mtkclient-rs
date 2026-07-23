//! Preloader 串口探测模式
//!
//! `zyb detect` — 等待 Preloader COM 口，打开后抓取所有串口数据输出到屏幕。
//! 用于分析 MTK Preloader UART 协议（如 MABT 的 CMD BootAsFASTBOOT）。

use log::info;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::preloader::transport::{BromPortResult, SerialPortTransport};

/// MTK Preloader 已知的命令字节（对齐 mtk_preloader.py Cmd 枚举）
const KNOWN_CMDS: &[(&str, u8)] = &[
    ("SEND_PARTITION_DATA", 0x70),
    ("JUMP_TO_PARTITION", 0x71),
    ("CHECK_USB_CMD", 0x72),
    ("STAY_STILL", 0x80),
    ("CMD_88", 0x88),
    ("CMD_READ16_A2", 0xA2),
    ("I2C_INIT", 0xB0),
    ("I2C_DEINIT / JUMP_MAUI", 0xB1),
    ("I2C_WRITE8", 0xB2),
    ("I2C_READ8", 0xB3),
    ("I2C_SET_SPEED", 0xB4),
    ("I2C_INIT_EX", 0xB6),
    ("I2C_WRITE8_EX / READY", 0xB8),
    ("I2C_READ8_EX", 0xB9),
    ("I2C_SET_SPEED_EX", 0xBA),
    ("GET_MAUI_FW_VER", 0xBF),
    ("OLD_SLA_SEND_AUTH", 0xC1),
    ("OLD_SLA_GET_RN", 0xC2),
    ("OLD_SLA_VERIFY_RN", 0xC3),
    ("PWR_INIT", 0xC4),
    ("PWR_DEINIT", 0xC5),
    ("PWR_READ16", 0xC6),
    ("PWR_WRITE16", 0xC7),
    ("CMD_C8 / EXT_CMD_GATE", 0xC8),
    ("READ16", 0xD0),
    ("READ32", 0xD1),
    ("WRITE16", 0xD2),
    ("WRITE16_NO_ECHO", 0xD3),
    ("WRITE32", 0xD4),
    ("JUMP_DA", 0xD5),
    ("JUMP_BL", 0xD6),
    ("SEND_DA", 0xD7),
    ("GET_TARGET_CONFIG", 0xD8),
    ("SEND_ENV_PREPARE", 0xD9),
    ("BROM_REG_ACCESS", 0xDA),
    ("UART1_LOG_EN", 0xDB),
    ("UART1_SET_BAUDRATE", 0xDC),
    ("BROM_DEBUGLOG", 0xDD),
    ("JUMP_DA64", 0xDE),
    ("GET_BROM_LOG_NEW", 0xDF),
    ("SEND_CERT", 0xE0),
    ("GET_ME_ID", 0xE1),
    ("SEND_AUTH", 0xE2),
    ("SLA", 0xE3),
    ("CMD_E4", 0xE4),
    ("CMD_E5", 0xE5),
    ("CMD_E6", 0xE6),
    ("GET_SOC_ID", 0xE7),
    ("CMD_E8", 0xE8),
    ("ZEROIZATION", 0xF0),
    ("GET_PL_CAP", 0xFB),
    ("CMD_FA", 0xFA),
    ("GET_HW_SW_VER", 0xFC),
    ("GET_HW_CODE", 0xFD),
    ("GET_BL_VER", 0xFE),
    ("GET_VERSION", 0xFF),
];

fn describe_cmd(byte: u8) -> String {
    for (name, cmd) in KNOWN_CMDS {
        if *cmd == byte {
            return format!("0x{:02X} ({})", byte, name);
        }
    }
    format!("0x{:02X} (未知)", byte)
}

/// 探测模式：等待 Preloader COM 口，抓取所有串口数据
pub fn cmd_detect() -> Result<(), Box<dyn std::error::Error>> {
    info!("=== Preloader 串口探测模式 ===");
    info!("等待 MTK Preloader 设备 (VID=0E8D, PID=2000)...");

    let port_result = loop {
        match SerialPortTransport::find_brom_port() {
            Some(r) => break r,
            None => {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };

    let port_name = match port_result {
        BromPortResult::SerialPort(name) => name,
        BromPortResult::WinUsbDevice => {
            return Err("检测到 WinUSB 设备，detect 需要 Preloader 串口模式".into());
        }
    };

    info!("找到 Preloader 设备: {}", port_name);
    info!("波特率: 115200, 8N1, 无流控");
    info!("");
    info!("=== 开始抓取串口数据 (Ctrl+C 退出) ===");
    info!("接收数据: << HEX | ASCII");
    info!("");
    info!("交互按键:");
    info!("  Enter     = 发送 Preloader 握手 (0xA0,0x0A,0x50,0x05)");
    info!("  h         = 发送 0xD6 (JUMP_BL)");
    info!("  f         = 发送 0xD6 (JUMP_BL) — 同 h");
    info!("  d         = 发送 0xD5 (JUMP_DA)");
    info!("  b         = 发送 0xD7 (SEND_DA)");
    info!("  8         = 发送 0xC8 (EXT_CMD_GATE)");
    info!("  q / Ctrl+C = 退出");
    info!("提示: 你可以用 MTK Bypass Tool 发送命令，观察协议帧");
    info!("      但注意串口独占，MABT 和本工具不能同时打开同一 COM 口");

    let mut port = serialport::new(&port_name, 115200)
        .timeout(Duration::from_millis(50))
        .data_bits(serialport::DataBits::Eight)
        .stop_bits(serialport::StopBits::One)
        .parity(serialport::Parity::None)
        .flow_control(serialport::FlowControl::None)
        .open()
        .map_err(|e| format!("无法打开串口 {}: {}", port_name, e))?;

    let mut read_buf = [0u8; 4096];
    let exit_flag = Arc::new(AtomicBool::new(false));

    // stdin 读取线程
    let exit_clone = Arc::clone(&exit_flag);
    let stdin_thread = std::thread::spawn(move || {
        let mut stdin_buf = [0u8; 1];
        let stdin = std::io::stdin();
        let mut stdin_lock = stdin.lock();
        loop {
            if exit_clone.load(Ordering::Relaxed) {
                break;
            }
            match stdin_lock.read(&mut stdin_buf) {
                Ok(1) => {
                    let ch = stdin_buf[0];
                    // 将按键通过 channel 传递给主线程
                    // 简化处理：直接在这里发送到串口
                    // （串口不能跨线程发送，所以用全局 stdin_cmd）
                    STDIN_CMD.store(ch as u8, Ordering::Release);
                }
                _ => break,
            }
        }
    });

    loop {
        // 非阻塞读取串口数据
        match port.read(&mut read_buf) {
            Ok(n) if n > 0 => {
                let data = &read_buf[..n];
                let hex: Vec<String> = data.iter().map(|b| format!("{:02X}", b)).collect();
                let ascii: String = data
                    .iter()
                    .map(|&b| {
                        if b.is_ascii_graphic() || b == b' ' {
                            b as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                eprintln!("<< [{}] {} | {}", n, hex.join(" "), ascii);
                // 尝试解析已知命令
                if n == 1 {
                    eprintln!("   解析: {}", describe_cmd(data[0]));
                }
            }
            _ => {}
        }

        // 检查 stdin 输入
        let cmd_byte = STDIN_CMD.swap(0, Ordering::AcqRel);
        if cmd_byte != 0 {
            match cmd_byte {
                b'\r' | b'\n' => {
                    let handshake = [0xA0u8, 0x0A, 0x50, 0x05];
                    eprintln!(
                        ">> 发送 Preloader 握手: {}",
                        handshake
                            .iter()
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    let _ = port.write_all(&handshake);
                    let _ = port.flush();
                }
                b'h' | b'f' => {
                    eprintln!(">> 发送 0xD6 (JUMP_BL)");
                    let _ = port.write_all(&[0xD6]);
                    let _ = port.flush();
                }
                b'd' => {
                    eprintln!(">> 发送 0xD5 (JUMP_DA)");
                    let _ = port.write_all(&[0xD5]);
                    let _ = port.flush();
                }
                b'b' => {
                    eprintln!(">> 发送 0xD7 (SEND_DA)");
                    let _ = port.write_all(&[0xD7]);
                    let _ = port.flush();
                }
                b'8' => {
                    eprintln!(">> 发送 0xC8 (EXT_CMD_GATE)");
                    let _ = port.write_all(&[0xC8]);
                    let _ = port.flush();
                }
                b'q' => {
                    eprintln!("\n退出探测模式");
                    break;
                }
                other => {
                    // 用户输入任意十六进制字符对，如 "DC" 发送 0xDC
                    eprintln!("   未知按键 0x{:02X} ('{}')，忽略", other, other as char);
                }
            }
        }

        // Ctrl+C 检测
        if crate::cancel::requested() {
            eprintln!("\n检测到 Ctrl+C，退出探测模式");
            break;
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    exit_flag.store(true, Ordering::Relaxed);
    let _ = stdin_thread.join();

    Ok(())
}

/// 全局 stdin 按键传递（stdin 线程 → 主线程）
static STDIN_CMD: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
