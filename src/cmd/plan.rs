//! `plan <op>`: inspect planning results offline (no device connection).
//!
//! Purpose (acceptance item 1): `mtkclient plan printgpt --loader DA.bin` shows
//! **at least two candidate paths + the rationale + a structured decision log** without `chain`.
//!
//! Read-only on the DA file; performs no device I/O.

use crate::core::capability::CapabilitySet;
use crate::core::device::{DeviceState, ProtocolVersion};
use crate::core::plan::Mode;
use crate::exploit::orchestrator::Orchestrator;
use crate::exploit::planner::Planner;
use crate::exploit::registry::ExploitRegistry;

/// Command entry point. `args` looks like `["printgpt", "--chip", "0x6768"]`.
pub fn cmd_plan(args: &[String], da_path: Option<&str>, cli_mode: &str) -> Result<(), String> {
    let op = args
        .first()
        .map(|s| s.as_str())
        .ok_or_else(|| crate::i18n::t("plan_usage").to_string())?;

    let required =
        crate::ops::required_caps(op).ok_or_else(|| crate::i18n::tr("plan_unknown_op", &[op]))?;

    // --da takes precedence over the global --loader
    let da_file = arg_value(args, "--da").or_else(|| da_path.map(|s| s.to_string()));
    let da = match da_file {
        Some(p) => Some(std::fs::read(&p).map_err(|e| format!("读取 {} 失败: {}", p, e))?),
        None => None,
    };
    let da_ref = da.as_deref();

    let da_code = arg_value(args, "--chip")
        .and_then(|s| parse_u16(&s))
        .or_else(|| da_ref.and_then(parse_da_code))
        .unwrap_or(0);

    let mut state = DeviceState {
        da_code,
        protocol: da_ref
            .map(ProtocolVersion::from_da_header)
            .unwrap_or(ProtocolVersion::Unknown),
        ..Default::default()
    };
    state.name = if da_code != 0 {
        format!("0x{:04X}", da_code)
    } else {
        String::new()
    };
    // Safety flags default to 0 (unreadable offline); users can simulate them with --safety 0x6
    if let Some(s) = arg_value(args, "--safety").and_then(|v| parse_u32(&v)) {
        state.safety = crate::core::safety::SafetyFlags::from_raw(s);
    }

    // plan defaults to auto mode (the global CLI --mode defaults to brom for the device flow);
    // detect whether --mode was passed explicitly and fall back to auto if not.
    let explicit_mode = std::env::args().any(|a| a == "--mode");
    let mode_str = if explicit_mode { cli_mode } else { "auto" };
    let mode =
        Mode::parse(mode_str).ok_or_else(|| crate::i18n::tr("plan_bad_mode", &[mode_str]))?;

    let planner = Planner::new(ExploitRegistry::builtin(), mode);
    let orch = Orchestrator::new(planner);
    let da_source = if da.is_some() { "file" } else { "none" };

    match orch.decide(
        op_static(op),
        required,
        CapabilitySet::EMPTY,
        &state,
        da_ref,
        da_source,
    ) {
        Ok(d) => {
            println!("{}", d.log.human());
            // The structured decision log is always printed (independent of --log level)
            println!("{}", d.log.json());
            Ok(())
        }
        Err(e) => {
            println!(
                "{}",
                Orchestrator::log_failure(
                    op_static(op),
                    required,
                    &e,
                    &state,
                    mode.label(),
                    da_source
                )
            );
            Err(e.to_string())
        }
    }
}

/// The op name must be `'static` (it is recorded in the decision log).
fn op_static(op: &str) -> &'static str {
    crate::ops::op_static_name(op)
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == key)?;
    args.get(pos + 1).cloned()
}

fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Parse `da_code` from a DA file (load table at 0x6C, each entry 0xD8/0xDC).
///
/// Matches the check in `da::loader::header`: read the u16 at [2..4] of each entry
/// and accept only the plausible range 0x6000..=0x8FFF.
fn parse_da_code(da: &[u8]) -> Option<u16> {
    const TABLE: usize = 0x6C;
    if da.len() < TABLE + 0xDC {
        return None;
    }
    for i in 0..8 {
        let base = TABLE + i * 0xDC;
        if base + 4 > da.len() {
            break;
        }
        let code = u16::from_le_bytes([da[base + 2], da[base + 3]]);
        if (0x6000..=0x8FFF).contains(&code) {
            return Some(code);
        }
    }
    // V6 DA may place hw_code elsewhere; fall back to scanning the first 4KB for 0x6xxx values
    let limit = da.len().min(0x1000);
    for w in da[..limit].windows(2) {
        let v = u16::from_le_bytes([w[0], w[1]]);
        if (0x6700..=0x8FFF).contains(&v) && v % 0x10 != 0 {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_caps_for_printgpt() {
        use crate::core::capability::Capability;
        let r = crate::ops::required_caps("printgpt").unwrap();
        assert!(r.contains(Capability::GptParsed));
        assert!(r.contains(Capability::StorageRead));
        assert!(crate::ops::required_caps("nope").is_none());
    }

    #[test]
    fn arg_parsing() {
        let args = vec![
            "printgpt".to_string(),
            "--chip".to_string(),
            "0x6768".to_string(),
        ];
        assert_eq!(arg_value(&args, "--chip").as_deref(), Some("0x6768"));
        assert_eq!(parse_u16("0x6768"), Some(0x6768));
        assert_eq!(parse_u16("26472"), Some(26472));
        assert_eq!(parse_u16("zzz"), None);
    }

    #[test]
    fn da_code_from_synthetic_header() {
        let mut da = vec![0u8; 0x1000];
        let base = 0x6C;
        da[base + 2..base + 4].copy_from_slice(&0x6768u16.to_le_bytes());
        assert_eq!(parse_da_code(&da), Some(0x6768));
        assert_eq!(parse_da_code(&[0u8; 8]), None);
    }

    #[test]
    fn unknown_op_rejected_without_da() {
        let err = cmd_plan(&["bogus".to_string()], None, "auto").unwrap_err();
        // Default locale is en-US, so the error text varies by language; just assert the op name is echoed
        assert!(err.contains("bogus"));
    }
}
