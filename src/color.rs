//! 自研终端着色（替代 colored crate）
//!
//! 仅实现项目实际用到的 13 个 `Colorize` 方法，全部为纯 ANSI 转义码，零外部依赖。
//! - 非 Windows 平台：终端原生支持 ANSI，直接输出即可。
//! - Windows 平台：需启用控制台虚拟终端处理（见 `enable_virtual_terminal`，
//!   复刻 `colored::control::set_virtual_terminal` 的行为），否则 CMD/PowerShell
//!   会把转义序列当作普通字符打印。
//!
//! 行为对齐：始终输出颜色（原项目调用 `colored::control::set_override(true)`
//! 强制开启），不做 TTY 检测，避免管道/日志场景下的行为漂移。

/// 所有着色方法返回的类型统一为 [`String`]，与 `colored` 的 `ColoredString`
/// 一样实现 `Display`，但比 `ColoredString` 更轻、且天然可参与 `String` 运算。
pub trait Colorize {
    fn black(self) -> String;
    fn red(self) -> String;
    fn green(self) -> String;
    fn yellow(self) -> String;
    fn blue(self) -> String;
    fn magenta(self) -> String;
    fn cyan(self) -> String;
    fn white(self) -> String;
    fn bright_white(self) -> String;
    fn bold(self) -> String;
    fn dimmed(self) -> String;
    fn underline(self) -> String;
    fn on_red(self) -> String;
    fn on_yellow(self) -> String;
}

const RESET: &str = "\x1b[0m";

/// 用指定 SGR 码包裹字符串（开码 … 文本 … 复位）。
///
/// 链式调用（如 `"x".red().bold()`）会产生多层转义，但首个 `RESET` 之前的属性
/// 均已生效，视觉结果正确，无副作用。
fn paint(s: &str, code: &str) -> String {
    format!("\x1b[{code}m{s}{RESET}")
}

/// 为所有 `AsRef<str>` 类型（`str` / `String` / `&str` / `&String` / `Cow<str>` 等）
/// 统一实现 `Colorize`。`self` 按值接收，但 `&str`/`&String` 均为 `Copy`，
/// 不会 move 底层数据；`String` 按值消费亦符合原 `colored` 语义。
macro_rules! impl_colorize {
    ($($name:ident => $code:literal),* $(,)?) => {
        impl<T: AsRef<str>> Colorize for T {
            $(fn $name(self) -> String { paint(self.as_ref(), $code) })*
        }
    };
}

impl_colorize! {
    black    => "30",
    red      => "31",
    green    => "32",
    yellow   => "33",
    blue     => "34",
    magenta  => "35",
    cyan     => "36",
    white    => "37",
    bright_white => "97",
    bold     => "1",
    dimmed   => "2",
    underline => "4",
    on_red   => "41",
    on_yellow => "43",
}

/// 启用 Windows 控制台虚拟终端处理，使 ANSI 转义序列在 CMD/PowerShell 中生效。
///
/// 复刻 `colored::control::set_virtual_terminal(true)`：对 stdout/stderr 控制台句柄
/// 追加 `ENABLE_VIRTUAL_TERMINAL_PROCESSING`。若句柄并非控制台（如被管道重定向），
/// `GetConsoleMode` 返回 0，安全跳过，不影响管道场景。
#[cfg(windows)]
pub fn enable_virtual_terminal() {
    use std::os::windows::io::AsRawHandle;

    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    unsafe {
        for h in [
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            let mut mode = 0u32;
            if GetConsoleMode(h as *mut std::ffi::c_void, &mut mode) != 0 {
                let _ = SetConsoleMode(
                    h as *mut std::ffi::c_void,
                    mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                );
            }
        }
    }
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetConsoleMode(hConsoleHandle: *mut std::ffi::c_void, lpMode: *mut u32) -> i32;
    fn SetConsoleMode(hConsoleHandle: *mut std::ffi::c_void, dwMode: u32) -> i32;
}

// 非 Windows 平台终端原生支持 ANSI，无需任何处理；main.rs 仅在
// `#[cfg(target_os = "windows")]` 分支调用 `enable_virtual_terminal`，
// 因此这里不再提供非 Windows 版本的空实现（避免成为 dead_code）。
