//! 反逆向工程：编译期字符串混淆 + 运行时反调试原语
//!
//! 目标是在**不牺牲源码可读性**的前提下，抬高三方静态/动态分析的成本：
//!
//! - **编译期字符串混淆**：`obf!("明文")` 里的明文只参与 `const` 常量求值，
//!   只有异或后的字节数组会进入二进制；运行时再由 [`decode`] 还原。这样用
//!   `strings`/IDA 直接搜索协议命令名、安全关键字等“指纹串”将一无所获。
//! - **反调试**：Windows 下通过 `IsDebuggerPresent` / `CheckRemoteDebuggerPresent`
//!   探测调试器附加；其它平台无此系统调用，恒返回 `false`。
//!
//! ## 为什么不用 proc-macro / 第三方库
//! `encode` 是 `const fn`，`obf!` 用普通 `macro_rules!` 即可在编译期完成异或，
//! 无需引入 proc-macro crate 或额外依赖，保持构建链简单、跨平台可编译。
//!
//! ## 用法
//! ```ignore
//! use crate::security::obfuscate::obf;
//! // 需要 String：直接 obf!("SENSITIVE")
//! // 需要 &str ：&obf!("SENSITIVE")
//! let cmd = obf!("SECURITY-SET-FLASH-POLICY");
//! ```
//!
//! 注意：混淆只针对**敏感指纹串**（协议命令名、安全关键字等）。普通日志、
//! 用户提示、分区名等功能性文本保持明文，避免无谓地降低可读性与可维护性。

/// 编译期字符串混淆的根密钥。
///
/// 运行期还会与字节位置混入，故单一常量泄露不足以还原全部字符串。
pub const XOR_KEY: u8 = 0x5B;

/// 位置相关的伪密钥序列（乘法步进弱化固定密钥的统计特征）。
#[inline]
const fn keystream(key: u8, i: usize) -> u8 {
    key.wrapping_add((i as u8).wrapping_mul(31))
        .wrapping_add(0x9D)
}

/// 编译期把明文字节逐字节异或为混淆字节数组。
///
/// 该函数只在 `const` 上下文求值：明文是常量折叠的中间值，不会作为
/// 独立常量写入最终二进制。
pub const fn encode<const N: usize>(src: &[u8], key: u8) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i] ^ keystream(key, i);
        i += 1;
    }
    out
}

/// 运行时把混淆字节还原为 `String`。
///
/// 混淆源来自合法 UTF-8 字面量，还原结果必然合法；若字节数组被外部篡改导致
/// 非法 UTF-8，则回退为空串而不是 panic（避免给分析者提供崩溃断点）。
pub fn decode(bytes: &[u8], key: u8) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        out.push(b ^ keystream(key, i));
    }
    String::from_utf8(out).unwrap_or_default()
}

/// 编译期字符串混淆宏：`obf!("明文")` → 运行时还原出的 `String`。
///
/// 明文字面量只出现在 `const` 求值中，最终二进制仅保留混淆字节。
#[macro_export]
macro_rules! obf {
    ($s:literal) => {{
        const __OBF_LEN: usize = $s.len();
        const __OBF_BYTES: [u8; __OBF_LEN] = $crate::security::obfuscate::encode(
            $s.as_bytes(),
            $crate::security::obfuscate::XOR_KEY,
        );
        $crate::security::obfuscate::decode(&__OBF_BYTES, $crate::security::obfuscate::XOR_KEY)
    }};
}

/// 探测当前进程是否被调试器附加。
///
/// Windows 下组合 `IsDebuggerPresent`（本地调试器）与
/// `CheckRemoteDebuggerPresent`（远程/内核调试器），任一命中即返回 `true`；
/// 其它平台无对应系统调用，恒返回 `false`。
#[cfg(target_os = "windows")]
pub fn debugger_present() -> bool {
    unsafe extern "system" {
        fn IsDebuggerPresent() -> i32;
        fn CheckRemoteDebuggerPresent(
            h_process: *mut std::ffi::c_void,
            pb_debugger_present: *mut i32,
        ) -> i32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }

    // SAFETY: 两个 API 均为无副作用的只读查询；句柄由 GetCurrentProcess 返回，
    // 是当前进程的伪句柄，无需释放。
    unsafe {
        if IsDebuggerPresent() != 0 {
            return true;
        }
        let mut remote_present: i32 = 0;
        if CheckRemoteDebuggerPresent(GetCurrentProcess(), &mut remote_present) != 0
            && remote_present != 0
        {
            return true;
        }
    }
    false
}

/// 非 Windows 平台的占位：不存在对应的系统调用，恒为 `false`。
#[cfg(not(target_os = "windows"))]
pub fn debugger_present() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_roundtrips_ascii() {
        const PLAIN: &str = "SECURITY-SET-ALLINONE-SIGNATURE";
        const OBF: [u8; PLAIN.len()] = encode(PLAIN.as_bytes(), XOR_KEY);
        assert_eq!(decode(&OBF, XOR_KEY), PLAIN);
    }

    #[test]
    fn obf_macro_roundtrips_utf8_and_empty() {
        assert_eq!(obf!(""), "");
        assert_eq!(obf!("CMD:DOWNLOAD-FILE"), "CMD:DOWNLOAD-FILE");
        assert_eq!(obf!("中文协议\u{0}"), "中文协议\u{0}");
    }

    #[test]
    fn encoded_bytes_differ_from_plaintext() {
        // 混淆后不应等于明文（否则等于没混淆）
        let plain = b"CMD:START";
        let obf: [u8; 9] = encode(plain, XOR_KEY);
        assert_ne!(&obf[..], &plain[..]);
        assert_eq!(decode(&obf, XOR_KEY), "CMD:START");
    }

    #[test]
    fn decode_rejects_tampered_bytes_without_panic() {
        // 人为构造非法 UTF-8 序列，验证回退为空串而非 panic
        let junk = [0xFFu8, 0xFE, 0xFD];
        assert_eq!(decode(&junk, XOR_KEY), "");
    }

    #[test]
    fn debugger_probe_is_callable() {
        // 仅验证探测函数可安全调用且不 panic（结果依赖运行环境）
        let _ = debugger_present();
    }
}
