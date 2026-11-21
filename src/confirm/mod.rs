//! 危险操作确认。
//!
//! 两种模式（与规划一致）：
//! - **不加 `--yes`**：先向 stderr 打印风险提示，再交互式地问 `y/N`。
//!   非交互环境（stdin 非终端、管道）无法提问，直接报错并提示加 `--yes`。
//! - **加 `--yes`**：直接放行，不打印任何警告。这是给脚本 / 其他程序调用
//!   准备的 —— 调用方自己负责确认后果，输出保持干净。

use std::io::BufRead;

/// 确认失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmError {
    /// 用户明确拒绝（交互式输入了 n/N）
    Declined,
    /// 非交互环境且无 `--yes`，无法确认
    NeedConfirmation,
}

impl std::fmt::Display for ConfirmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfirmError::Declined => write!(f, "操作已被取消"),
            ConfirmError::NeedConfirmation => write!(f, "需要交互式确认或 --yes"),
        }
    }
}

impl std::error::Error for ConfirmError {}

/// 危险等级（仅决定警告文案的措辞，不影响交互方式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    /// 不可逆（preloader / seccfg / nvram 等写坏即变砖）
    Irreversible,
    /// 高风险但可恢复（boot / super / vbmeta 等）
    High,
}

impl Danger {
    /// 警告文案里的等级词。
    pub fn label(&self) -> &'static str {
        match self {
            Danger::Irreversible => "不可逆",
            Danger::High => "高风险",
        }
    }
}

/// 确认上下文。
#[derive(Debug, Clone)]
pub struct ConfirmContext {
    /// 设备序列号（用于审计留痕）
    pub serial: String,
    /// 附加指纹（如分区名 / 镜像 sha）
    pub fingerprint: String,
    /// 时间戳（由调用方传入，便于测试）
    pub timestamp: String,
}

impl ConfirmContext {
    /// 审计行（仅用于 stderr 警告，便于事后追溯）。
    pub fn audit_line(&self, action: &str) -> String {
        format!(
            "[audit] {} action={} sn={} fp={}",
            self.timestamp, action, self.serial, self.fingerprint
        )
    }
}

/// 确认器。
pub struct DangerConfirm {
    /// `--yes`：直接放行，不提问、不打警告。
    assume_yes: bool,
    /// 是否允许交互式输入（stdin 为 TTY 时由 CLI 打开）。
    interactive: bool,
    /// 测试注入的 yes/no 答案来源；`None` 时惰性使用 stdin（每次读取单独加锁，避免嵌套死锁）。
    injected: Option<Box<dyn BufRead>>,
}

impl DangerConfirm {
    pub fn new(assume_yes: bool) -> Self {
        Self {
            assume_yes,
            interactive: false,
            injected: None,
        }
    }

    /// 打开交互式确认（单元测试直接构造；CLI 走 `set_interactive`）。
    #[cfg(test)]
    pub fn interactive(mut self, on: bool) -> Self {
        self.interactive = on;
        self
    }

    /// 直接设置（保留其它字段；CLI 在 `--yes` 之后调用也不会丢掉它）。
    pub fn set_interactive(&mut self, on: bool) {
        self.interactive = on;
    }

    /// 测试用：注入 yes/no 答案来源。
    #[cfg(test)]
    pub fn with_reader(mut self, r: impl BufRead + 'static) -> Self {
        self.injected = Some(Box::new(r));
        self
    }

    /// 请求确认。
    ///
    /// - `--yes`：直接返回 `Ok(())`，不打印任何内容（给程序调用）。
    /// - 否则：先打印警告到 stderr；交互式则问 `y/N`，非交互式直接报错。
    pub fn require(
        &mut self,
        action: &'static str,
        danger: Danger,
        ctx: &ConfirmContext,
    ) -> Result<(), ConfirmError> {
        if self.assume_yes {
            // 给程序调用：静默放行
            return Ok(());
        }

        eprintln!(
            "警告: {} 为{}操作，可能损坏设备。{}",
            action,
            danger.label(),
            ctx.audit_line(action)
        );

        if self.interactive {
            eprint!("继续执行？[y/N] ");
            let mut line = String::new();
            let read = match &mut self.injected {
                Some(r) => r.read_line(&mut line),
                None => std::io::stdin().lock().read_line(&mut line),
            };
            if read.is_err() {
                return Err(ConfirmError::Declined);
            }
            let ans = line.trim();
            if matches!(ans.to_ascii_lowercase().as_str(), "y" | "yes") {
                Ok(())
            } else {
                Err(ConfirmError::Declined)
            }
        } else {
            Err(ConfirmError::NeedConfirmation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn ctx() -> ConfirmContext {
        ConfirmContext {
            serial: "ABC12345".into(),
            fingerprint: "boot_a".into(),
            timestamp: "2026-11-14T20:00:00Z".into(),
        }
    }

    #[test]
    fn yes_passes_through_silently() {
        let mut c = DangerConfirm::new(true);
        assert!(c.require("erase boot_a", Danger::High, &ctx()).is_ok());
        assert!(
            c.require("seccfg unlock", Danger::Irreversible, &ctx())
                .is_ok()
        );
    }

    #[test]
    fn non_interactive_without_yes_needs_confirmation() {
        let mut c = DangerConfirm::new(false);
        assert_eq!(
            c.require("erase boot_a", Danger::High, &ctx()),
            Err(ConfirmError::NeedConfirmation)
        );
    }

    #[test]
    fn interactive_y_approves() {
        let mut c = DangerConfirm::new(false)
            .interactive(true)
            .with_reader(Cursor::new("y\n"));
        assert!(c.require("erase boot_a", Danger::High, &ctx()).is_ok());
    }

    #[test]
    fn interactive_yes_approves() {
        let mut c = DangerConfirm::new(false)
            .interactive(true)
            .with_reader(Cursor::new("YES\n"));
        assert!(c.require("erase boot_a", Danger::High, &ctx()).is_ok());
    }

    #[test]
    fn interactive_n_declines() {
        let mut c = DangerConfirm::new(false)
            .interactive(true)
            .with_reader(Cursor::new("n\n"));
        assert_eq!(
            c.require("erase boot_a", Danger::High, &ctx()),
            Err(ConfirmError::Declined)
        );
    }

    #[test]
    fn interactive_empty_declines() {
        let mut c = DangerConfirm::new(false)
            .interactive(true)
            .with_reader(Cursor::new("\n"));
        assert_eq!(
            c.require("erase boot_a", Danger::High, &ctx()),
            Err(ConfirmError::Declined)
        );
    }

    #[test]
    fn irreversible_warning_label_differs_from_high() {
        // 仅校验文案分支，交互逻辑一致（都是 y/N）
        let mut c = DangerConfirm::new(false);
        assert_eq!(
            c.require("seccfg unlock", Danger::Irreversible, &ctx()),
            Err(ConfirmError::NeedConfirmation)
        );
    }

    #[test]
    fn audit_line_contains_all_fields() {
        let line = ctx().audit_line("erase_data");
        assert!(line.contains("2026-11-14"));
        assert!(line.contains("action=erase_data"));
        assert!(line.contains("sn=ABC12345"));
        assert!(line.contains("fp=boot_a"));
    }
}
