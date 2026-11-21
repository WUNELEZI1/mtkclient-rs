//! CLI output internationalization (i18n).
//!
//! Defaults to `en-US`; supports `en/en-US`, `zh-CN`, `fr/fr-FR`, `ru/ru-RU`, `es/es-ES`, `ar/ar-SA`.
//! Language source priority: command-line `--language <LANG>` (at any position,
//! e.g. `mtkclient-rs help --language zh-CN`) > `MTK_LANG` > `LC_ALL`/`LANG`.
//! Note: raw text returned by the device is not translated; the tool's own progress logs are being
//! migrated to this module by frequency priority (connect / DA load / partition read-write / seccfg / RPMB done); unmigrated deep debug logs remain in Chinese.

use std::sync::atomic::{AtomicUsize, Ordering};

mod messages;
use messages::{help_text as lang_help, table as lang_table};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En = 0,
    ZhCn = 1,
    Fr = 2,
    Ru = 3,
    Es = 4,
    Ar = 5,
}

const LANG_COUNT: usize = 6;
static CURRENT: AtomicUsize = AtomicUsize::new(Lang::En as usize);

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::SeqCst) {
        1 => Lang::ZhCn,
        2 => Lang::Fr,
        3 => Lang::Ru,
        4 => Lang::Es,
        5 => Lang::Ar,
        _ => Lang::En,
    }
}

pub fn parse(s: &str) -> Option<Lang> {
    let v = s.trim().to_ascii_lowercase().replace('_', "-");
    match v.as_str() {
        "en" | "en-us" | "en-gb" | "english" => Some(Lang::En),
        "zh" | "zh-cn" | "zh-hans" | "zh-sg" | "chinese" => Some(Lang::ZhCn),
        "fr" | "fr-fr" | "fr-ca" | "french" => Some(Lang::Fr),
        "ru" | "ru-ru" | "russian" => Some(Lang::Ru),
        "es" | "es-es" | "es-mx" | "es-ar" | "spanish" => Some(Lang::Es),
        "ar" | "ar-sa" | "ar-eg" | "arabic" => Some(Lang::Ar),
        _ => None,
    }
}

fn set(l: Lang) {
    CURRENT.store(l as usize, Ordering::SeqCst);
}

/// Extract `--language/--lang` (at any position) and return the remaining arguments.
pub fn extract_from_args(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let inline = a
            .strip_prefix("--language=")
            .or_else(|| a.strip_prefix("--lang="));
        if let Some(v) = inline {
            if let Some(l) = parse(v) {
                set(l);
            } else {
                eprintln!("mtkclient-rs: warning: unsupported language '{v}', using en-US");
            }
            i += 1;
        } else if a == "--language" || a == "--lang" {
            if let Some(v) = args.get(i + 1) {
                if let Some(l) = parse(v) {
                    set(l);
                } else {
                    eprintln!("mtkclient-rs: warning: unsupported language '{v}', using en-US");
                }
                i += 2;
            } else {
                eprintln!("mtkclient-rs: warning: --language requires a value, using en-US");
                i += 1;
            }
        } else {
            out.push(a.clone());
            i += 1;
        }
    }
    out
}

/// Environment fallback: `MTK_LANG` > `LC_ALL` > `LANG`.
pub fn detect_env() {
    for var in ["MTK_LANG", "LC_ALL", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let head = v.split(['.', '@']).next().unwrap_or("");
            if let Some(l) = parse(head) {
                set(l);
                return;
            }
        }
    }
}

const KEYS: &[&str] = &[
    "os_unsupported",
    "arch_unsupported",
    "os_warning",
    "get_updates",
    "free_tool_notice",
    "data_dir_missing",
    "elevate_request",
    "elevate_failed",
    "elevate_hint",
    "connected_preloader",
    "connected_brom",
    "no_preloader_forced_dump",
    "adb_rebooting",
    "unknown_command",
    "usage_r_gpt",
    "usage_rl",
    "usage_wl",
    "usage_w_gpt",
    "read_gpt_failed",
    "gpt_unavailable",
    "parse_gpt_failed",
    "no_super_in_gpt",
    // ── Deep progress logs (connect / DA / partition read-write / seccfg) ──────────────────
    "log_wait_device",
    "log_auto_detect",
    "log_brom_usb_found",
    "log_brom_com_ok",
    "log_da_loaded",
    "log_da_ext_ready",
    "log_skip_existing",
    "log_saved_to",
    "log_write_start",
    "log_write_done",
    "log_verify_ok",
    "log_erase_start",
    "log_erased",
    "log_format_start",
    "log_format_done",
    "log_unlock_start",
    "log_unlock_ok",
    "log_lock_start",
    "log_lock_ok",
    "log_rpmb_key_ok",
    "plan_usage",
    "plan_unknown_op",
    "plan_bad_mode",
];

pub fn t(key: &str) -> &'static str {
    let idx = lang() as usize;
    debug_assert!(idx < LANG_COUNT);
    let table = lang_table(idx);
    for (i, k) in KEYS.iter().enumerate() {
        if *k == key {
            return table[i];
        }
    }
    ""
}

pub fn tr(key: &str, args: &[&str]) -> String {
    let mut s = t(key).to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

/// Full help text for the current language.
pub fn help() -> &'static str {
    lang_help(lang() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!(parse("en-US"), Some(Lang::En));
        assert_eq!(parse("zh-CN"), Some(Lang::ZhCn));
        assert_eq!(parse("zh_TW"), None);
        assert_eq!(parse("fr"), Some(Lang::Fr));
        assert_eq!(parse("ru-RU"), Some(Lang::Ru));
        assert_eq!(parse("es"), Some(Lang::Es));
        assert_eq!(parse("ar-SA"), Some(Lang::Ar));
    }

    #[test]
    fn extract_language_anywhere() {
        let rest = extract_from_args(vec!["help".into(), "--language".into(), "zh-CN".into()]);
        assert_eq!(rest, vec!["help".to_string()]);
        assert_eq!(lang(), Lang::ZhCn);
        set(Lang::En);
    }

    #[test]
    fn catalog_is_complete() {
        let catalogs: [&[&str]; LANG_COUNT] = [
            lang_table(0),
            lang_table(1),
            lang_table(2),
            lang_table(3),
            lang_table(4),
            lang_table(5),
        ];
        for catalog in catalogs {
            assert_eq!(catalog.len(), KEYS.len());
        }
        for (i, key) in KEYS.iter().enumerate() {
            // Placeholder counts must match across languages, otherwise tr() replacement would break
            let count = catalogs[0][i].matches('{').count();
            for (lang_idx, catalog) in catalogs.iter().enumerate() {
                let text = catalog[i];
                assert!(
                    !text.trim().is_empty(),
                    "key={} lang_idx={} 翻译为空",
                    key,
                    lang_idx
                );
                assert_eq!(
                    text.matches('{').count(),
                    count,
                    "key={} lang_idx={} 占位符数量不一致",
                    key,
                    lang_idx
                );
            }
        }
    }

    #[test]
    fn log_keys_are_localized() {
        for key in ["log_da_loaded", "log_wait_device", "log_write_done"] {
            assert!(!t(key).is_empty(), "缺少 key: {}", key);
        }
        set(Lang::En);
        assert_eq!(t("log_da_loaded"), "DA loaded");
        set(Lang::ZhCn);
        assert_eq!(t("log_da_loaded"), "DA 加载完成");
        set(Lang::Fr);
        assert_eq!(t("log_da_loaded"), "DA chargé");
        set(Lang::En);
        assert!(tr("log_write_done", &["1024", "boot"]).contains("boot"));
    }

    #[test]
    fn help_mentions_sim_in_all_langs() {
        for l in [Lang::En, Lang::ZhCn, Lang::Fr, Lang::Ru, Lang::Es, Lang::Ar] {
            set(l);
            assert!(help().contains("--sim"), "缺少 --sim: {:?}", l);
            assert!(help().contains("mock://"), "缺少 mock://: {:?}", l);
        }
        set(Lang::En);
    }

    #[test]
    fn help_mentions_language_in_all_langs() {
        for l in [Lang::En, Lang::ZhCn, Lang::Fr, Lang::Ru, Lang::Es, Lang::Ar] {
            set(l);
            assert!(help().contains("mtkclient-rs"));
            assert!(help().contains("--language"));
        }
        set(Lang::En);
    }
}
