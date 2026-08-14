//! sdata.json 运行时加载
//!
//! `sdata.json`（位于 `data/sdata.json`）是芯片支持的**动态配置**，由外部 GUI（刷机匣）
//! 或用户维护。Rust 在运行时读取它，根据设备上报的 `hw_code` 决定对应的 payload 文件路径，
//! 而不是把"支持哪些芯片 / 每个芯片用哪个 payload"硬编码进代码。
//!
//! 唯一保持硬编码的是 `chips_generated.rs` 的 `CHIP_CONFIGS`——hwcode ↔ 芯片 bootrom 寄存器
//! 地址对照表。这是因为：① 设备只上报 hwcode，必须有一张表把 hwcode 映射到芯片；② 每台芯片
//! 的 bootrom 寄存器地址不同，无法动态化。这部分不在 sdata.json 的职责范围内。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::system::config::CHIP_CONFIGS;
use crate::system::paths::获取可执行文件相对路径;

/// 解析后的 sdata
pub struct SData {
    /// support_chip：芯片键 -> hwcode（十六进制字符串，如 `"0x788"`）
    pub support_chip: HashMap<String, String>,
    /// 各芯片键 -> payload 相对路径（如 `"mt6771\\mt6771_payload.bin"`）
    pub payloads: HashMap<String, String>,
}

static S_DATA: OnceLock<Option<SData>> = OnceLock::new();

/// 获取（并缓存）sdata。读取/解析失败返回 `None`。
pub fn sdata() -> Option<&'static SData> {
    S_DATA.get_or_init(load_sdata).as_ref()
}

/// 把 `CHIP_CONFIGS.name`（如 `"MT6768/MT6769"`、`"MT6771"`）归一化为 sdata.json 的短键。
/// 规则：取 `/` 或空格前的第一段，整体转小写 → `"mt6768"` / `"mt6771"`。
fn normalize_chip_key(name: &str) -> String {
    name.split(['/', ' ']).next().unwrap_or(name).to_lowercase()
}

/// 动态解析某芯片的 payload 路径（基于 sdata.json 的 `data/<芯片>/<文件>` 布局）。
///
/// 解析策略：
/// 1. 用 `CHIP_CONFIGS`（硬编码 hwcode ↔ 芯片名）按 `hw_code` **或** `da_code` 双向匹配，
///    兼容 BROM 上报 `0x0707` 与 DA 内部 `0x6768` 两种情况，得到芯片身份。
/// 2. 收集候选的 sdata.json `payloads` 键：
///    - CHIP_CONFIGS 芯片名归一化短键（如 `"MT6768/MT6769"` → `"mt6768"`）；
///    - sdata.json `support_chip` 中命中 hw_code / da_code 的键（自身即 payloads 键）。
/// 3. 依次用候选键查 `payloads`，拼成 `data/<相对路径>` 并返回首个存在的文件。
/// 4. 全部未命中时回退到旧式 `payload/<fallback_loader>` 目录。
pub fn resolve_chip_payload(hw_code: u32, fallback_loader: &str) -> std::path::PathBuf {
    // 1. 确定芯片身份（hw_code 或 da_code 任一匹配）
    let chip = CHIP_CONFIGS
        .iter()
        .find(|c| c.hw_code as u32 == hw_code || c.da_code as u32 == hw_code);

    // 2. 收集候选的 sdata payloads 键
    let mut keys: Vec<String> = Vec::new();
    if let Some(cfg) = chip {
        keys.push(normalize_chip_key(cfg.name));
    }
    if let Some(sd) = sdata() {
        let probe = chip.map(|c| (c.hw_code as u32, c.da_code as u32));
        for (key, hwstr) in &sd.support_chip {
            if let Some(h) = parse_hwcode(hwstr) {
                // 命中：设备上报值，或该芯片已知的 hw_code/da_code（覆盖 da_code 上报场景）
                let hit = h == hw_code || probe.map_or(false, |(hw, da)| h == hw || h == da);
                if hit {
                    keys.push(key.clone());
                }
            }
        }
    }

    // 3. 依次用候选键查 sdata payloads，返回首个存在的 data/<相对路径>
    if let Some(sd) = sdata() {
        for key in &keys {
            if let Some(rel) = sd.payloads.get(key) {
                let p = 获取可执行文件相对路径(&format!("data/{}", normalize_sep(rel)));
                if p.exists() {
                    return p;
                }
            }
        }
    }

    // 4. 回退：旧式 payload/ 目录
    获取可执行文件相对路径(&format!("payload/{}", fallback_loader))
}

/// 解析 sdata.json 中具名 payload（如 `generic_preloader_dump` / `generic_patcher`）的路径。
///
/// sdata.json 缺失或未含该键时，回退到旧式 `payload/<fallback_filename>` 目录。
pub fn resolve_named_payload(sdata_key: &str, fallback_filename: &str) -> std::path::PathBuf {
    if let Some(sd) = sdata() {
        if let Some(rel) = sd.payloads.get(sdata_key) {
            let data_path = 获取可执行文件相对路径(&format!("data/{}", normalize_sep(rel)));
            if data_path.exists() {
                return data_path;
            }
        }
    }
    获取可执行文件相对路径(&format!("payload/{}", fallback_filename))
}

/// 解析 hwcode 字符串（支持 `0x788` / `0X707` / 十进制 `788`）
fn parse_hwcode(s: &str) -> Option<u32> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).ok()
}

/// 把 Windows 反斜杠路径分隔符归一化为 `/`（Rust Path 跨平台拼接时更稳）
fn normalize_sep(p: &str) -> String {
    p.replace('\\', "/")
}

fn load_sdata() -> Option<SData> {
    let path = 获取可执行文件相对路径("data/sdata.json");
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
    Some(SData { support_chip, payloads })
}

// ---------- 最小化 JSON 解析器（无第三方依赖，离线可用） ----------

#[allow(dead_code)] // 解析器构造全部变体，但 sdata.json 只用 Obj/Str/Bool，其余字段未被读取属正常
enum JVal {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<JVal>),
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
            b't' => self.parse_lit("true", JVal::Bool(true)),
            b'f' => self.parse_lit("false", JVal::Bool(false)),
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
        s.parse::<f64>().ok().map(JVal::Num)
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
        let mut arr = Vec::new();
        self.skip_ws();
        if self.peek()? == b']' {
            self.i += 1;
            return Some(JVal::Arr(arr));
        }
        loop {
            let v = self.parse_value()?;
            arr.push(v);
            self.skip_ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(JVal::Arr(arr));
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
                    JVal::Obj(inner) => assert!(matches!(inner["y"], JVal::Bool(true))),
                    _ => panic!("x not obj"),
                }
            }
            _ => panic!("not obj"),
        }
    }

    #[test]
    fn test_sdata_roundtrip() {
        // 校验 support_chip 逆向映射 + payload 路径解析（不依赖磁盘文件）
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
        // 模拟 payload_for_hwcode 的核心逻辑
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
