use std::io::Write;
use std::sync::Mutex;

/// Get a local timestamp string [YYYY/MM/DD HH:MM:SS.mmm]
fn get_local_timestamp() -> String {
    #[cfg(target_os = "windows")]
    {
        #[repr(C)]
        struct SystemTime {
            w_year: u16,
            w_month: u16,
            w_day_of_week: u16,
            w_day: u16,
            w_hour: u16,
            w_minute: u16,
            w_second: u16,
            w_milliseconds: u16,
        }
        unsafe extern "system" {
            fn GetLocalTime(lpSystemTime: *mut SystemTime);
        }
        let mut st = SystemTime {
            w_year: 0,
            w_month: 0,
            w_day_of_week: 0,
            w_day: 0,
            w_hour: 0,
            w_minute: 0,
            w_second: 0,
            w_milliseconds: 0,
        };
        unsafe {
            GetLocalTime(&mut st);
        }
        format!(
            "{:04}/{:02}/{:02} {:02}:{:02}:{:02}.{:03}",
            st.w_year, st.w_month, st.w_day, st.w_hour, st.w_minute, st.w_second, st.w_milliseconds
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let millis = now.subsec_millis();
        let hours = (secs % 86400) / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
    }
}

/// In-house terminal logger, replacing env_logger::Logger.
/// Mirrors the original env_logger terminal format: `timestamp` `LEVEL` message, keeping per-module filtering (nusb capped at Warn).
pub(crate) struct TerminalLogger {
    /// Global default log level (mtkclient_rs and other modules)
    pub(crate) level: log::LevelFilter,
}

impl TerminalLogger {
    /// Return the effective level for a target: nusb is forced to at most Warn, others use the global default.
    fn level_for(&self, target: &str) -> log::LevelFilter {
        if target == "nusb" {
            log::LevelFilter::Warn
        } else {
            self.level
        }
    }
}

impl log::Log for TerminalLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARN ",
            log::Level::Info => "INFO ",
            log::Level::Debug => "DEBUG",
            log::Level::Trace => "TRACE",
        };
        let timestamp = get_local_timestamp();
        let _ = writeln!(
            std::io::stderr(),
            "[{}] [{}] {}",
            timestamp,
            level,
            record.args()
        );
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// TeeLogger: write to both the terminal (TerminalLogger) and a file (usb_debug.log)
pub(crate) struct TeeLogger {
    pub(crate) terminal: TerminalLogger,
    pub(crate) file: Mutex<std::fs::File>,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.terminal.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        // 1. Terminal output (keeps the original format)
        self.terminal.log(record);

        // 2. File output (simplified format including target for troubleshooting)
        let line = format!(
            "[{}] [{}] [{}] {}\n",
            get_local_timestamp(),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        self.terminal.flush();
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}
