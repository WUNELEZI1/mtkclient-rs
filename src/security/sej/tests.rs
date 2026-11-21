use super::*;

fn patterned(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(7).wrapping_add(3)) as u8)
        .collect()
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn xor_g_hacc_cfg_1_is_self_inverse() {
    // XOR with a constant is its own inverse; verify for lengths below,
    // at, and above the 16-byte window that the helper operates on.
    for &len in &[0, 1, 4, 15, 16, 17, 32, 33, 64] {
        let original = patterned(len);
        let mut data = original.clone();
        xor_g_hacc_cfg_1(&mut data);
        xor_g_hacc_cfg_1(&mut data);
        assert_eq!(
            data, original,
            "double XOR must restore input (len={})",
            len
        );
    }
}

#[test]
fn xor_g_hacc_cfg_1_leaves_tail_beyond_16_bytes_unchanged() {
    // The helper is capped at 16 bytes; everything after that is untouched.
    let original = patterned(48);
    let mut data = original.clone();
    xor_g_hacc_cfg_1(&mut data);
    assert_eq!(&data[16..], &original[16..]);
}

#[test]
fn sw_encrypt_decrypt_roundtrip() {
    // Block-aligned inputs must survive encrypt -> decrypt byte-for-byte.
    // A custom key is used to exercise the key parameter of both helpers.
    let key = [0xA5u8; 32];
    for &len in &[16, 32, 48, 64, 160] {
        let plaintext = patterned(len);
        let ciphertext = sej_sec_cfg_sw_encrypt_with_key(&plaintext, &key).unwrap();
        assert_eq!(ciphertext.len(), len, "ciphertext length (len={})", len);
        assert_ne!(
            ciphertext, plaintext,
            "ciphertext must differ (len={})",
            len
        );
        let recovered = sej_sec_cfg_sw_decrypt_with_key(&ciphertext, &key).unwrap();
        assert_eq!(recovered, plaintext, "roundtrip mismatch (len={})", len);
    }
}

#[test]
fn sw_encrypt_known_vector() {
    // Regression vectors: AES-256-CBC with key = SEJ_SW_KEY and IV = SEJ_IV,
    // NoPadding, frozen from the current implementation and cross-checked
    // against OpenSSL. Do not "fix" these values if an implementation
    // change alters the output; that is exactly what this test detects.
    let key = sw_key_default();
    let cases: &[(&[u8], &str)] = &[
        (b"0123456789abcdef", "db2d71492847d3322e7676dc9b660d3d"),
        (
            b"0123456789abcdefABCDEF0123456789",
            "db2d71492847d3322e7676dc9b660d3d2f888abe5a354f8eff0c00aefc663b36",
        ),
    ];
    for &(plaintext, expected_hex) in cases {
        let ciphertext = sej_sec_cfg_sw_encrypt_with_key(plaintext, &key).unwrap();
        assert_eq!(ciphertext, from_hex(expected_hex));
        // Decrypting the frozen ciphertext reproduces the original input.
        let recovered = sej_sec_cfg_sw_decrypt_with_key(&ciphertext, &key).unwrap();
        assert_eq!(recovered, plaintext);
    }
}

#[test]
fn sw_decrypt_rejects_non_block_aligned_length() {
    // Decrypt must reject any length that is not a multiple of 16.
    let key = sw_key_default();
    for &len in &[1, 15, 17, 31, 33] {
        let data = vec![0x00u8; len];
        let err = sej_sec_cfg_sw_decrypt_with_key(&data, &key).unwrap_err();
        assert!(
            err.contains("16"),
            "unexpected error (len={}): {}",
            len,
            err
        );
    }
}

#[test]
fn sw_encrypt_zero_pads_to_block_boundary() {
    // Encrypt zero-pads non-aligned input up to the AES block size.
    let key = sw_key_default();
    let plaintext = b"abc";
    let ciphertext = sej_sec_cfg_sw_encrypt_with_key(plaintext, &key).unwrap();
    assert_eq!(ciphertext.len(), 16);
    let recovered = sej_sec_cfg_sw_decrypt_with_key(&ciphertext, &key).unwrap();
    assert_eq!(&recovered[..plaintext.len()], plaintext);
    assert!(recovered[plaintext.len()..].iter().all(|&b| b == 0));
}
