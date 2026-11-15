//! 危险操作确认。
//!
//! 规则（与规划一致）：
//! - `rpmb set-key`：**每次**都要 token（设备 SN 后 4 位），不缓存；
//! - `rpmb write` / `seccfg unlock` / `erase_data`：同会话确认一次；
//! - `--yes`：跳过交互，但**警告文本仍返回给调用方打印到 stderr**（验收项 7）；
//! - 所有确认都带时间戳、SN、密钥指纹，供日志留痕。

use std::collections::HashSet;

/// 确认失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmError {
    /// 需要输入 token 但未提供
    NeedToken,
    /// token 不匹配
    TokenMismatch,
    /// 用户拒绝
    Declined,
}

impl std::fmt::Display for ConfirmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfirmError::NeedToken => write!(f, "需要确认 token（设备 SN 后 4 位）"),
            ConfirmError::TokenMismatch => write!(f, "确认 token 不匹配"),
            ConfirmError::Declined => write!(f, "操作已被取消"),
        }
    }
}

impl std::error::Error for ConfirmError {}

/// 危险等级（决定是否需要每次确认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    /// 不可逆（RPMB 密钥烧写）
    Irreversible,
    /// 高风险但可恢复（unlock / erase_data / rpmb write）
    High,
}

/// 确认上下文。
#[derive(Debug, Clone)]
pub struct ConfirmContext {
    /// 设备序列号（token 取后 4 位）
    pub serial: String,
    /// 附加指纹（如密钥 SHA256 前 8 位）
    pub fingerprint: String,
    /// 时间戳（由调用方传入，便于测试）
    pub timestamp: String,
}

impl ConfirmContext {
    pub fn token(&self) -> String {
        let s = self.serial.trim();
        if s.len() <= 4 {
            s.to_ascii_uppercase()
        } else {
            s[s.len() - 4..].to_ascii_uppercase()
        }
    }

    /// 留痕行。
    pub fn audit_line(&self, action: &str) -> String {
        format!(
            "[audit] {} action={} sn={} token={} fp={}",
            self.timestamp,
            action,
            self.serial,
            self.token(),
            self.fingerprint
        )
    }
}

/// 确认器。
pub struct DangerConfirm {
    assume_yes: bool,
    /// 是否允许交互式输入（stdin 为 TTY 时由 CLI 打开）。
    /// 测试与非交互环境保持 false，行为完全确定。
    interactive: bool,
    approved: HashSet<&'static str>,
}

impl DangerConfirm {
    pub fn new(assume_yes: bool) -> Self {
        Self {
            assume_yes,
            interactive: false,
            approved: HashSet::new(),
        }
    }

    /// 打开交互式确认（仅在 stdin 是终端时由 CLI 调用）。
    pub fn interactive(mut self, on: bool) -> Self {
        self.interactive = on;
        self
    }

    pub fn assume_yes(&self) -> bool {
        self.assume_yes
    }

    /// 请求确认。
    ///
    /// 返回 `Ok(Some(warning))` 表示放行，但调用方**必须**把 warning 打到 stderr
    /// （`--yes` 也不省略）。`Ok(None)` 表示无需提示。
    pub fn require(
        &mut self,
        action: &'static str,
        danger: Danger,
        ctx: &ConfirmContext,
        input_token: Option<&str>,
    ) -> Result<Option<String>, ConfirmError> {
        let warning = Some(format!(
            "警告: {} 为{}操作，{}",
            action,
            if danger == Danger::Irreversible {
                "不可逆"
            } else {
                "高风险"
            },
            ctx.audit_line(action)
        ));

        if self.assume_yes {
            // 跳过交互，但警告照常返回
            return Ok(warning);
        }

        // 交互式终端：直接问 y/N（不可逆操作仍要求输入 token，避免误按回车）
        if self.interactive && input_token.is_none() {
            if danger == Danger::Irreversible {
                eprint!("{}", warning.as_deref().unwrap_or(""));
                eprintln!("\n请输入设备 SN 后 4 位确认（{}）: ", ctx.token());
            } else {
                eprint!("{}", warning.as_deref().unwrap_or(""));
                eprint!("继续执行？[y/N] ");
            }
            use std::io::BufRead;
            let mut line = String::new();
            if std::io::stdin().lock().read_line(&mut line).is_err() {
                return Err(ConfirmError::Declined);
            }
            let ans = line.trim();
            if danger == Danger::Irreversible {
                if !ans.eq_ignore_ascii_case(&ctx.token()) {
                    return Err(ConfirmError::TokenMismatch);
                }
            } else if !matches!(ans.to_ascii_lowercase().as_str(), "y" | "yes") {
                return Err(ConfirmError::Declined);
            }
            if danger == Danger::High {
                self.approved.insert(action);
            }
            return Ok(warning);
        }

        // Irreversible：每次都要 token
        if danger == Danger::Irreversible {
            let Some(tok) = input_token else {
                return Err(ConfirmError::NeedToken);
            };
            if !tok.eq_ignore_ascii_case(&ctx.token()) {
                return Err(ConfirmError::TokenMismatch);
            }
            return Ok(warning);
        }

        // High：同会话一次
        if self.approved.contains(action) {
            return Ok(None);
        }
        if let Some(tok) = input_token {
            if !tok.eq_ignore_ascii_case(&ctx.token()) {
                return Err(ConfirmError::TokenMismatch);
            }
        } else {
            return Err(ConfirmError::NeedToken);
        }
        self.approved.insert(action);
        Ok(warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ConfirmContext {
        ConfirmContext {
            serial: "ABC12345".into(),
            fingerprint: "deadbeef".into(),
            timestamp: "2026-11-14T20:00:00Z".into(),
        }
    }

    #[test]
    fn token_takes_last_four_uppercase() {
        assert_eq!(ctx().token(), "2345");
    }

    #[test]
    fn yes_skips_interaction_but_keeps_warning() {
        let mut c = DangerConfirm::new(true);
        let w = c
            .require("rpmb set-key", Danger::Irreversible, &ctx(), None)
            .unwrap();
        let w = w.expect("--yes 仍必须返回警告");
        assert!(w.contains("警告"));
        assert!(w.contains("不可逆"));
        assert!(w.contains("sn=ABC12345"));
    }

    #[test]
    fn irreversible_needs_token_every_time() {
        let mut c = DangerConfirm::new(false);
        assert_eq!(
            c.require("rpmb set-key", Danger::Irreversible, &ctx(), None),
            Err(ConfirmError::NeedToken)
        );
        assert_eq!(
            c.require("rpmb set-key", Danger::Irreversible, &ctx(), Some("0000")),
            Err(ConfirmError::TokenMismatch)
        );
        assert!(
            c.require("rpmb set-key", Danger::Irreversible, &ctx(), Some("2345"))
                .is_ok()
        );
        // 第二次仍需 token
        assert_eq!(
            c.require("rpmb set-key", Danger::Irreversible, &ctx(), None),
            Err(ConfirmError::NeedToken)
        );
    }

    #[test]
    fn high_only_once_per_session() {
        let mut c = DangerConfirm::new(false);
        assert!(
            c.require("seccfg unlock", Danger::High, &ctx(), Some("2345"))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            c.require("seccfg unlock", Danger::High, &ctx(), None),
            Ok(None),
            "同会话第二次应免确认"
        );
    }

    #[test]
    fn non_interactive_without_token_says_need_token() {
        let mut c = DangerConfirm::new(false);
        assert_eq!(
            c.require("write boot_a", Danger::High, &ctx(), None),
            Err(ConfirmError::NeedToken)
        );
    }

    #[test]
    fn audit_line_contains_all_fields() {
        let line = ctx().audit_line("erase_data");
        assert!(line.contains("2026-11-14"));
        assert!(line.contains("action=erase_data"));
        assert!(line.contains("sn=ABC12345"));
        assert!(line.contains("fp=deadbeef"));
    }
}
