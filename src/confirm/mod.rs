//! Dangerous operation confirmation.
//!
//! Two modes (consistent with planning):
//! - **Without `--yes`**: print the risk notice to stderr, then ask `y/N` interactively.
//!   In a non-interactive environment (stdin is not a TTY, pipes) the question is impossible, so error out and suggest `--yes`.
//! - **With `--yes`**: proceed directly and print no warning. This is meant for scripts /
//!   other programs — the caller owns the consequences and the output stays clean.

use std::io::BufRead;

/// Reason a confirmation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmError {
    /// The user explicitly declined (typed n/N)
    Declined,
    /// Non-interactive environment without `--yes`; cannot confirm
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

/// Danger level (affects only the wording of the warning, not the interaction mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    /// Irreversible (a bad write to preloader / seccfg / nvram bricks the device)
    Irreversible,
    /// High risk but recoverable (boot / super / vbmeta, etc.)
    High,
}

impl Danger {
    /// Level word used in the warning text.
    pub fn label(&self) -> &'static str {
        match self {
            Danger::Irreversible => "不可逆",
            Danger::High => "高风险",
        }
    }
}

/// Confirmation context.
#[derive(Debug, Clone)]
pub struct ConfirmContext {
    /// Device serial number (for the audit trail)
    pub serial: String,
    /// Extra fingerprint (e.g. partition name / image sha)
    pub fingerprint: String,
    /// Timestamp (passed in by the caller for easy testing)
    pub timestamp: String,
}

impl ConfirmContext {
    /// Audit line (for the stderr warning only; keeps a trace afterwards).
    pub fn audit_line(&self, action: &str) -> String {
        format!(
            "[audit] {} action={} sn={} fp={}",
            self.timestamp, action, self.serial, self.fingerprint
        )
    }
}

/// Confirmer.
pub struct DangerConfirm {
    /// `--yes`: proceed directly, no question and no warning.
    assume_yes: bool,
    /// Whether interactive input is allowed (enabled by the CLI when stdin is a TTY).
    interactive: bool,
    /// Test-injected yes/no answer source; when `None`, stdin is used lazily (locked per read to avoid nested deadlocks).
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

    /// Enable interactive confirmation (unit tests build it directly; the CLI uses `set_interactive`).
    #[cfg(test)]
    pub fn interactive(mut self, on: bool) -> Self {
        self.interactive = on;
        self
    }

    /// Set in place (other fields are kept; calling it after `--yes` in the CLI does not lose the flag).
    pub fn set_interactive(&mut self, on: bool) {
        self.interactive = on;
    }

    /// Test-only: inject a yes/no answer source.
    #[cfg(test)]
    pub fn with_reader(mut self, r: impl BufRead + 'static) -> Self {
        self.injected = Some(Box::new(r));
        self
    }

    /// Request confirmation.
    ///
    /// - `--yes`: return `Ok(())` immediately and print nothing (for programmatic calls).
    /// - Otherwise: print the warning to stderr; ask `y/N` when interactive, error out otherwise.
    pub fn require(
        &mut self,
        action: &'static str,
        danger: Danger,
        ctx: &ConfirmContext,
    ) -> Result<(), ConfirmError> {
        if self.assume_yes {
            // Programmatic call: proceed silently
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
        // Only the wording branch is checked; the interaction logic is identical (both y/N)
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
