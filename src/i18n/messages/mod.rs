//! Per-language message tables and help text.
//!
//! The order of `MESSAGES` in submodules must strictly match `super::KEYS`.

pub(super) mod ar;
pub(super) mod en;
pub(super) mod es;
pub(super) mod fr;
pub(super) mod ru;
pub(super) mod zh_cn;

/// Returns the message table for a language index (index matches `Lang` discriminant).
pub(super) fn table(idx: usize) -> &'static [&'static str] {
    match idx {
        1 => zh_cn::MESSAGES,
        2 => fr::MESSAGES,
        3 => ru::MESSAGES,
        4 => es::MESSAGES,
        5 => ar::MESSAGES,
        _ => en::MESSAGES,
    }
}

/// Returns the full help text for a language index.
pub(super) fn help_text(idx: usize) -> &'static str {
    match idx {
        1 => zh_cn::HELP,
        2 => fr::HELP,
        3 => ru::HELP,
        4 => es::HELP,
        5 => ar::HELP,
        _ => en::HELP,
    }
}
