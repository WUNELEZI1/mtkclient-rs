//! Lightweight in-house progress bar (replaces the indicatif crate).
//!
//! Implements only the API actually used by the project:
//! `ProgressBar::{hidden,new,set_style,set_message,set_position,finish_with_message,
//! finish_and_clear,abandon_with_message}` + `Clone`.
//! Renders to stderr and reuses the `color` module for styling; stays silent when
//! stderr is not a console (redirected) or when hidden, to avoid polluting
//! logs/piped output (mirrors indicatif's TTY detection).

use crate::color::Colorize;
use crate::util::display_width;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// End-of-line clear sequence (erase cursor to end of line).
const CLEAR_EOL: &str = "\x1b[K";

/// Mutable internal state of the bar. Shared via `Arc<Mutex>` so `ProgressBar` is
/// `Clone` and `Send+Sync`.
struct BarState {
    total: u64,
    position: u64,
    message: String,
    start: Instant,
    template: String,
    /// Progress chars: `0`=filled `1`=partial `2`=blank
    pchars: [char; 3],
    hidden: bool,
    finished: bool,
    spinner: u64,
}

impl BarState {
    fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.position as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }

    fn percent(&self) -> u32 {
        if self.total == 0 {
            0
        } else {
            ((self.position as f64 / self.total as f64) * 100.0).clamp(0.0, 100.0) as u32
        }
    }

    /// Average speed (bytes/sec) = transferred / elapsed.
    fn speed_per_sec(&self) -> f64 {
        let secs = self.start.elapsed().as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.position as f64 / secs
        }
    }
}

/// Progress bar style (replaces `indicatif::ProgressStyle`).
///
/// `with_template` returns `Result` to keep the call-site chain working
/// (`ProgressStyle::with_template(t).unwrap().progress_chars("█▓░")`).
pub struct ProgressStyle {
    template: String,
    pchars: [char; 3],
}

impl ProgressStyle {
    pub fn with_template(template: &str) -> Result<Self, &'static str> {
        Ok(Self {
            template: template.to_string(),
            pchars: ['█', '▓', '░'],
        })
    }

    pub fn progress_chars(mut self, chars: &str) -> Self {
        let mut it = chars.chars();
        if let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) {
            self.pchars = [a, b, c];
        }
        self
    }
}

/// Lightweight progress bar (replaces `indicatif::ProgressBar`).
#[derive(Clone)]
pub struct ProgressBar {
    inner: Arc<Mutex<BarState>>,
}

impl ProgressBar {
    /// Hidden progress bar: all rendering is silent (used by `--quiet-dump` / `QUIET_USB_READ`).
    pub fn hidden() -> Self {
        ProgressBar {
            inner: Arc::new(Mutex::new(BarState {
                total: 0,
                position: 0,
                message: String::new(),
                start: Instant::now(),
                template: default_template(),
                pchars: ['█', '▓', '░'],
                hidden: true,
                finished: false,
                spinner: 0,
            })),
        }
    }

    /// Creates a progress bar with a total length of `len` bytes.
    pub fn new(len: u64) -> Self {
        ProgressBar {
            inner: Arc::new(Mutex::new(BarState {
                total: len,
                position: 0,
                message: String::new(),
                start: Instant::now(),
                template: default_template(),
                pchars: ['█', '▓', '░'],
                hidden: false,
                finished: false,
                spinner: 0,
            })),
        }
    }

    pub fn set_style(&self, style: ProgressStyle) {
        if let Ok(mut s) = self.inner.lock() {
            s.template = style.template;
            s.pchars = style.pchars;
        }
    }

    pub fn set_message(&self, msg: impl Into<String>) {
        if let Ok(mut s) = self.inner.lock() {
            s.message = msg.into();
            self.draw(&s);
        }
    }

    pub fn set_position(&self, pos: u64) {
        if let Ok(mut s) = self.inner.lock() {
            s.position = pos;
            s.spinner = s.spinner.wrapping_add(1);
            self.draw(&s);
        }
    }

    pub fn finish_with_message(&self, msg: impl Into<String>) {
        if let Ok(mut s) = self.inner.lock() {
            s.message = msg.into();
            s.finished = true;
            self.finalize(&s);
        }
    }

    pub fn finish_and_clear(&self) {
        if let Ok(mut s) = self.inner.lock() {
            s.finished = true;
            let hidden = s.hidden;
            if hidden || !stderr_is_console() {
                return;
            }
            drop(s);
            let _ = std::io::stderr().write_all(b"\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }

    pub fn abandon_with_message(&self, msg: impl Into<String>) {
        if let Ok(mut s) = self.inner.lock() {
            s.message = msg.into();
            s.finished = true;
            self.finalize(&s);
        }
    }

    /// Renders the current state on the same stderr line (carriage return overwrite,
    /// history not cleared).
    fn draw(&self, s: &BarState) {
        if s.hidden || s.finished || !stderr_is_console() {
            return;
        }
        // First render the rest with bar_width=0 to measure its visible width, then
        // derive the bar width from the terminal width.
        let rest = expand(&s.template, s, 0);
        let rest_w = display_width(&strip_ansi(&rest));
        let bar_w = terminal_width().saturating_sub(rest_w).max(8);
        let line = expand(&s.template, s, bar_w);
        let _ = std::io::stderr().write_all(b"\r");
        let _ = write!(std::io::stderr(), "{}{}", line, CLEAR_EOL);
        let _ = std::io::stderr().flush();
    }

    /// Finish rendering: clear the progress line and print the final message on its own line.
    fn finalize(&self, s: &BarState) {
        if s.hidden || !stderr_is_console() {
            return;
        }
        let _ = std::io::stderr().write_all(b"\r\x1b[K");
        let _ = writeln!(std::io::stderr(), "{}", s.message);
        let _ = std::io::stderr().flush();
    }
}

/// Default template (matching existing call sites: spinner + elapsed + bar + bytes/percent + message).
fn default_template() -> String {
    "  {spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] \
     {binary_bytes}/{binary_total_bytes} ({percent}%) {msg}"
        .to_string()
}

/// Expands the template into a visible string. `bar_width` is the bar placeholder width
/// (0 means measure the rest first).
fn expand(template: &str, s: &BarState, bar_width: usize) -> String {
    let bytes = template.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            i += 1;
            let mut token = String::new();
            while i < bytes.len() && bytes[i] != b'}' {
                token.push(bytes[i] as char);
                i += 1;
            }
            i += 1; // skip '}'
            let name = match token.split_once(':') {
                Some((n, _)) => n, // color modifiers (e.g. .green / .cyan/blue) are ignored here; colors are hardcoded in render_token
                None => token.as_str(),
            };
            out.push_str(&render_token(name, s, bar_width));
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Renders a single placeholder.
fn render_token(name: &str, s: &BarState, bar_width: usize) -> String {
    match name {
        "spinner" => {
            const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let f = FRAMES[(s.spinner as usize) % FRAMES.len()];
            f.to_string().green()
        }
        "elapsed_precise" => fmt_duration(s.start.elapsed()),
        "eta" => fmt_eta(s),
        "wide_bar" => build_bar(s, bar_width),
        "binary_bytes" => fmt_binary(s.position as f64),
        "binary_total_bytes" => fmt_binary(s.total as f64),
        "binary_bytes_per_sec" => format!("{}/s", fmt_binary(s.speed_per_sec())),
        "percent" => format!("{}", s.percent()),
        "msg" => s.message.clone(),
        _ => String::new(),
    }
}

/// Builds the progress bar string of width `width` (filled=cyan, blank=blue).
fn build_bar(s: &BarState, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let frac = s.fraction();
    let pos = frac * width as f64;
    let full = pos.floor() as usize;
    let partial = pos - full as f64;
    let mut out = String::with_capacity(width * 12);
    for j in 0..width {
        let ch = if j < full {
            s.pchars[0]
        } else if j == full && partial > 0.0 && full < width {
            s.pchars[1]
        } else {
            s.pchars[2]
        };
        if ch == s.pchars[2] {
            out.push_str(&ch.to_string().blue());
        } else {
            out.push_str(&ch.to_string().cyan());
        }
    }
    out
}

/// Human-readable binary byte count (B / KiB / MiB / GiB ...), matching indicatif's
/// compact style (no space).
fn fmt_binary(bytes: f64) -> String {
    if bytes < 1024.0 {
        return format!("{}B", bytes as i64);
    }
    let units = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = bytes / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.2}{}", v, units[i])
}

/// Formats a duration as `MM:SS.cc` (with `H:` prefix when >=1h), matching indicatif's
/// `elapsed_precise`.
fn fmt_duration(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let sec = total % 60;
    let cs = d.subsec_millis() / 10;
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}.{cs:02}")
    } else {
        format!("{m:02}:{sec:02}.{cs:02}")
    }
}

/// Estimated remaining time; returns `--:--` when speed is 0 or nothing has started.
fn fmt_eta(s: &BarState) -> String {
    if s.position == 0 {
        return "--:--".to_string();
    }
    let speed = s.speed_per_sec();
    if speed <= 0.0 {
        return "--:--".to_string();
    }
    let remaining = (s.total.saturating_sub(s.position)) as f64 / speed;
    fmt_duration(Duration::from_secs_f64(remaining))
}

/// Removes ANSI escape sequences (used for visible-width measurement).
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            while i < bytes.len() && bytes[i] != b'm' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // skip 'm'
            }
        } else {
            let len = utf8_len(bytes[i]);
            if i + len <= bytes.len() {
                out.push_str(&s[i..i + len]);
            }
            i += len;
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

#[cfg(windows)]
#[repr(C)]
struct COORD {
    x: i16,
    y: i16,
}

#[cfg(windows)]
#[repr(C)]
struct SMALL_RECT {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[cfg(windows)]
#[repr(C)]
struct CONSOLE_SCREEN_BUFFER_INFO {
    size: COORD,
    cursor_position: COORD,
    attributes: u16,
    window: SMALL_RECT,
    maximum_window_size: COORD,
}

#[cfg(windows)]
fn terminal_width() -> usize {
    const STD_ERROR_HANDLE: i32 = -12;
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h.is_null() {
            return 80;
        }
        let mut info = std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFO>();
        if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let w = info.size.x as i32;
            if w > 0 {
                return w as usize;
            }
        }
    }
    80
}

#[cfg(not(windows))]
fn terminal_width() -> usize {
    80
}

/// Whether stderr is a console (not redirected). When not a console the bar stays
/// silent to avoid polluting logs/pipes.
#[cfg(windows)]
fn stderr_is_console() -> bool {
    const STD_ERROR_HANDLE: i32 = -12;
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h.is_null() {
            return false;
        }
        let mut mode = 0u32;
        GetConsoleMode(h, &mut mode) != 0
    }
}

#[cfg(not(windows))]
fn stderr_is_console() -> bool {
    true
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetStdHandle(nStdHandle: i32) -> *mut std::ffi::c_void;
    fn GetConsoleScreenBufferInfo(
        hConsoleOutput: *mut std::ffi::c_void,
        lpConsoleScreenBufferInfo: *mut CONSOLE_SCREEN_BUFFER_INFO,
    ) -> i32;
    fn GetConsoleMode(hConsoleHandle: *mut std::ffi::c_void, lpMode: *mut u32) -> i32;
}
