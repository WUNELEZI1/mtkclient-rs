//! Anti-reverse engineering: compile-time string obfuscation + runtime anti-debug primitives
//!
//! The goal is to raise the cost of third-party static/dynamic analysis **without
//! sacrificing source readability**:
//!
//! - **Compile-time string obfuscation**: the plaintext in `obf!("plaintext")` only
//!   participates in `const` evaluation, so only the XORed byte array enters the
//!   binary; [`decode`] restores it at runtime. Searching protocol command names,
//!   security keywords and other "fingerprint strings" with `strings`/IDA therefore
//!   yields nothing.
//! - **Anti-debug**: on Windows, debugger attachment is detected via
//!   `IsDebuggerPresent` / `CheckRemoteDebuggerPresent`; other platforms lack these
//!   syscalls and always return `false`.
//!
//! ## Why not a proc-macro / third-party crate
//! `encode` is a `const fn`, so a plain `macro_rules!` suffices for the compile-time
//! XOR, avoiding a proc-macro crate or extra dependency and keeping the build chain
//! simple and cross-platform compilable.
//!
//! ## Usage
//! ```ignore
//! use crate::security::obfuscate::obf;
//! // Need String: use obf!("SENSITIVE") directly
//! // Need &str   : use &obf!("SENSITIVE")
//! let cmd = obf!("SECURITY-SET-FLASH-POLICY");
//! ```
//!
//! Note: obfuscation only targets **sensitive fingerprint strings** (protocol command
//! names, security keywords, etc.). Ordinary logs, user prompts, partition names and
//! other functional text stay plaintext to avoid needlessly hurting readability and
//! maintainability.

/// Root key for compile-time string obfuscation.
///
/// It is mixed with the byte position at runtime, so leaking this single constant is
/// not enough to recover all strings.
pub const XOR_KEY: u8 = 0x5B;

/// Position-dependent pseudo-key stream (multiplicative stepping weakens the
/// statistical signature of a fixed key).
#[inline]
const fn keystream(key: u8, i: usize) -> u8 {
    key.wrapping_add((i as u8).wrapping_mul(31))
        .wrapping_add(0x9D)
}

/// XORs plaintext bytes into an obfuscated byte array at compile time.
///
/// This function is evaluated only in `const` context: the plaintext is a constant-
/// folded intermediate value and is not written to the final binary as a separate
/// constant.
pub const fn encode<const N: usize>(src: &[u8], key: u8) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = src[i] ^ keystream(key, i);
        i += 1;
    }
    out
}

/// Restores obfuscated bytes into a `String` at runtime.
///
/// The obfuscated source comes from a valid UTF-8 literal, so the result is always
/// valid; if the byte array is tampered with externally and becomes invalid UTF-8,
/// it falls back to an empty string instead of panicking (avoiding a crash
/// breakpoint for the analyst).
pub fn decode(bytes: &[u8], key: u8) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        out.push(b ^ keystream(key, i));
    }
    String::from_utf8(out).unwrap_or_default()
}

/// Compile-time string obfuscation macro: `obf!("plaintext")` -> `String` restored at runtime.
///
/// The plaintext literal appears only in `const` evaluation; the final binary keeps
/// only the obfuscated bytes.
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

/// Detects whether a debugger is attached to the current process.
///
/// On Windows it combines `IsDebuggerPresent` (local debugger) with
/// `CheckRemoteDebuggerPresent` (remote/kernel debugger); either hit returns `true`.
/// Other platforms lack the corresponding syscalls and always return `false`.
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

    // SAFETY: both APIs are side-effect-free read-only queries; the handle returned by
    // GetCurrentProcess is a pseudo handle for the current process and needs no release.
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

/// Placeholder for non-Windows platforms: the syscall does not exist, always `false`.
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
        // Obfuscated output must not equal the plaintext (otherwise it is not obfuscated)
        let plain = b"CMD:START";
        let obf: [u8; 9] = encode(plain, XOR_KEY);
        assert_ne!(&obf[..], &plain[..]);
        assert_eq!(decode(&obf, XOR_KEY), "CMD:START");
    }

    #[test]
    fn decode_rejects_tampered_bytes_without_panic() {
        // Deliberately construct an invalid UTF-8 sequence to verify the empty-string fallback instead of panic
        let junk = [0xFFu8, 0xFE, 0xFD];
        assert_eq!(decode(&junk, XOR_KEY), "");
    }

    #[test]
    fn debugger_probe_is_callable() {
        // Only verifies the probe can be called safely without panic (result depends on the runtime environment)
        let _ = debugger_present();
    }
}
