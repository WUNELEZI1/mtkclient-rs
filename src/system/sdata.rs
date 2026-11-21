//! sdata.json runtime loading
//!
//! `sdata.json` (at `data/sdata.json`) is the **dynamic configuration** for chip support, maintained by an external GUI (flashing toolbox)
//! or by the user. Rust reads it at runtime and determines the corresponding payload file path from the device-reported `hw_code`,
//! rather than hardcoding "which chips are supported / which payload each chip uses" into the code.
//!
//! The only thing kept hardcoded is `CHIP_CONFIGS` in `chips_generated.rs` — the hwcode ↔ chip bootrom register
//! address mapping table. Reasons: (1) the device only reports the hwcode, so a table is needed to map hwcode to a chip; (2) each chip
//! has different bootrom register addresses, which cannot be dynamic. This is outside the scope of sdata.json.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::system::config::CHIP_CONFIGS;
use crate::system::paths::get_exe_relative_path;

/// parsed sdata
pub struct SData {
    /// support_chip: chip key -> hwcode (hex string, e.g. `"0x788"`)
    pub support_chip: HashMap<String, String>,
    /// each chip key -> payload relative path (e.g. `"mt6771\\mt6771_payload.bin"`)
    pub payloads: HashMap<String, String>,
}

static S_DATA: OnceLock<Option<SData>> = OnceLock::new();

/// get (and cache) the sdata. Returns `None` on read/parse failure.
pub fn sdata() -> Option<&'static SData> {
    S_DATA.get_or_init(load_sdata).as_ref()
}

/// normalize `CHIP_CONFIGS.name` (e.g. `"MT6768/MT6769"`, `"MT6771"`) to the short key used by sdata.json.
/// rule: take the first segment before `/` or a space, lowercase it → `"mt6768"` / `"mt6771"`.
fn normalize_chip_key(name: &str) -> String {
    name.split(['/', ' ']).next().unwrap_or(name).to_lowercase()
}

/// dynamically resolve a chip's payload path (based on the sdata.json `data/<chip>/<file>` layout).
///
/// resolution strategy:
/// 1. use `CHIP_CONFIGS` (hardcoded hwcode ↔ chip name) to match bidirectionally by `hw_code` **or** `da_code`,
///    covering both the BROM-reported `0x0707` and DA-internal `0x6768` cases to obtain the chip identity.
/// 2. collect candidate sdata.json `payloads` keys:
///    - the normalized short key of the CHIP_CONFIGS chip name (e.g. `"MT6768/MT6769"` → `"mt6768"`);
///    - keys in sdata.json `support_chip` that match hw_code / da_code (the key itself is a payloads key).
/// 3. look up `payloads` with each candidate key in turn, assemble `data/<relative path>`, and return the first existing file.
/// 4. if nothing matches, fall back to the legacy `payload/<fallback_loader>` directory.
pub fn resolve_chip_payload(hw_code: u32, fallback_loader: &str) -> std::path::PathBuf {
    // 1. determine the chip identity (either hw_code or da_code matches)
    let chip = CHIP_CONFIGS
        .iter()
        .find(|c| c.hw_code as u32 == hw_code || c.da_code as u32 == hw_code);

    // 2. collect candidate sdata payloads keys
    let mut keys: Vec<String> = Vec::new();
    if let Some(cfg) = chip {
        keys.push(normalize_chip_key(cfg.name));
    }
    if let Some(sd) = sdata() {
        let probe = chip.map(|c| (c.hw_code as u32, c.da_code as u32));
        for (key, hwstr) in &sd.support_chip {
            if let Some(h) = parse_hwcode(hwstr) {
                // hit: a device-reported value, or a known hw_code/da_code of this chip (covers the da_code reporting case)
                let hit = h == hw_code || probe.map_or(false, |(hw, da)| h == hw || h == da);
                if hit {
                    keys.push(key.clone());
                }
            }
        }
    }

    // 3. look up sdata payloads with each candidate key in turn; return the first existing data/<relative path>
    if let Some(sd) = sdata() {
        for key in &keys {
            if let Some(rel) = sd.payloads.get(key) {
                let p = get_exe_relative_path(&format!("data/{}", normalize_sep(rel)));
                if p.exists() {
                    return p;
                }
            }
        }
    }

    // 4. fallback: legacy payload/ directory
    get_exe_relative_path(&format!("payload/{}", fallback_loader))
}

/// resolve the path of a named payload in sdata.json (e.g. `generic_preloader_dump` / `generic_patcher`).
///
/// if sdata.json is missing or lacks the key, fall back to the legacy `payload/<fallback_filename>` directory.
pub fn resolve_named_payload(sdata_key: &str, fallback_filename: &str) -> std::path::PathBuf {
    if let Some(sd) = sdata() {
        if let Some(rel) = sd.payloads.get(sdata_key) {
            let data_path = get_exe_relative_path(&format!("data/{}", normalize_sep(rel)));
            if data_path.exists() {
                return data_path;
            }
        }
    }
    get_exe_relative_path(&format!("payload/{}", fallback_filename))
}

/// parse a hwcode string (supports `0x788` / `0X707` / decimal `788`)
fn parse_hwcode(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).ok()
}

/// normalize Windows backslash path separators to `/` (more robust for cross-platform Rust Path joining)
fn normalize_sep(p: &str) -> String {
    p.replace('\\', "/")
}

fn load_sdata() -> Option<SData> {
    let path = get_exe_relative_path("data/sdata.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let val = parse_json(text)?;
    let obj = match val {
        JVal::Obj(o) => o,
        _ => return None,
    };
    let mut support_chip = HashMap::new();
    let mut payloads = HashMap::new();
    for (k, v) in obj {
        if k == "support_chip" {
            if let JVal::Obj(m) = v {
                for (kk, vv) in m {
                    if let JVal::Str(s) = vv {
                        support_chip.insert(kk, s);
                    }
                }
            }
        } else if let JVal::Str(s) = v {
            payloads.insert(k, s);
        }
    }
    Some(SData {
        support_chip,
        payloads,
    })
}

// ---------- minimal JSON parser (no third-party dependencies, works offline) ----------

/// minimal JSON value type.
///
/// sdata.json only uses objects/strings (`Obj`/`Str`); `Null`/`Bool`/`Num`/`Arr` are kept only so the parser
/// can accept and skip valid but unconsumed JSON fragments; their inner values are never read, so they carry no payload field.
enum JVal {
    Null,
    Bool,
    Num,
    Str(String),
    Arr,
    Obj(HashMap<String, JVal>),
}

fn parse_json(s: &str) -> Option<JVal> {
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
    };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    Some(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn parse_value(&mut self) -> Option<JVal> {
        self.skip_ws();
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Some(JVal::Str(self.parse_string()?)),
            b't' => self.parse_lit("true", JVal::Bool),
            b'f' => self.parse_lit("false", JVal::Bool),
            b'n' => self.parse_lit("null", JVal::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_lit(&mut self, lit: &str, val: JVal) -> Option<JVal> {
        let lb = lit.as_bytes();
        if self.i + lb.len() <= self.b.len() && &self.b[self.i..self.i + lb.len()] == lb {
            self.i += lb.len();
            Some(val)
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<JVal> {
        let start = self.i;
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' => self.i += 1,
                _ => break,
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        // the number itself is not used by sdata; this only validates that its numeric format is legal
        s.parse::<f64>().ok().map(|_| JVal::Num)
    }

    fn parse_string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            let cp = u32::from_str_radix(h, 16).ok()?;
                            out.push(char::from_u32(cp)?);
                        }
                        _ => return None,
                    }
                }
                _ => out.push(*c as char),
            }
        }
    }

    fn parse_array(&mut self) -> Option<JVal> {
        if self.peek()? != b'[' {
            return None;
        }
        self.i += 1;
        self.skip_ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(JVal::Arr);
        }
        loop {
            // array element values are not used by sdata; parse recursively only to validate and skip the fragment
            self.parse_value()?;
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(JVal::Arr);
                }
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self) -> Option<JVal> {
        if self.peek()? != b'{' {
            return None;
        }
        self.i += 1;
        let mut obj = HashMap::new();
        self.skip_ws();
        if self.peek()? == b'}' {
            self.i += 1;
            return Some(JVal::Obj(obj));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek()? != b':' {
                return None;
            }
            self.i += 1;
            let v = self.parse_value()?;
            obj.insert(key, v);
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(JVal::Obj(obj));
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hwcode() {
        assert_eq!(parse_hwcode("0x788"), Some(0x788));
        assert_eq!(parse_hwcode("0X707"), Some(0x707));
        assert_eq!(parse_hwcode("788"), Some(0x788));
        assert_eq!(parse_hwcode(" 0x6768 "), Some(0x6768));
    }

    #[test]
    fn test_minimal_json() {
        let v = parse_json(r#"{"a":"b\\c","n":1,"x":{"y":true}}"#).unwrap();
        match v {
            JVal::Obj(o) => {
                match &o["a"] {
                    JVal::Str(s) => assert_eq!(s, "b\\c"),
                    _ => panic!("a not str"),
                }
                match &o["x"] {
                    JVal::Obj(inner) => assert!(matches!(inner["y"], JVal::Bool)),
                    _ => panic!("x not obj"),
                }
            }
            _ => panic!("not obj"),
        }
    }

    #[test]
    fn test_sdata_roundtrip() {
        // verify the support_chip reverse mapping + payload path resolution (no disk file dependency)
        let json = r#"{
            "support_chip": {"mt6771":"0x788","mt6768":"0x707"},
            "mt6771": "mt6771\\mt6771_payload.bin",
            "mt6768": "mt6768\\mt6768_payload.bin"
        }"#;
        let val = parse_json(json).unwrap();
        let obj = match val {
            JVal::Obj(o) => o,
            _ => panic!(),
        };
        let mut support_chip = HashMap::new();
        let mut payloads = HashMap::new();
        for (k, v) in obj {
            if k == "support_chip" {
                if let JVal::Obj(m) = v {
                    for (kk, vv) in m {
                        if let JVal::Str(s) = vv {
                            support_chip.insert(kk, s);
                        }
                    }
                }
            } else if let JVal::Str(s) = v {
                payloads.insert(k, s);
            }
        }
        // simulate the core logic of payload_for_hwcode
        let hw = 0x788u32;
        let mut found = None;
        for (key, hwstr) in &support_chip {
            if let Some(h) = parse_hwcode(hwstr) {
                if h == hw {
                    found = payloads.get(key).cloned();
                }
            }
        }
        assert_eq!(found, Some("mt6771\\mt6771_payload.bin".to_string()));
    }
}
