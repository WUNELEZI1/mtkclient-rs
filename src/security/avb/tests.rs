use super::*;

/// Build a minimal valid vbmeta: header + 1 hash descriptor (inside the aux block).
fn make_test_vbmeta() -> Vec<u8> {
    // descriptor: tag=3(Hash), len = payload size
    let name = b"boot";
    let d_payload_len = 8 + 32 + 4 + 4 + 4 + 4 + name.len();
    let mut d = Vec::new();
    d.extend_from_slice(&3u64.to_be_bytes());
    d.extend_from_slice(&(d_payload_len as u64).to_be_bytes());
    d.extend_from_slice(&0x1000u64.to_be_bytes()); // image_size
    d.extend_from_slice(&[0u8; 32]); // hash algorithm "sha256" padded? lenient test
    d.extend_from_slice(&(name.len() as u32).to_be_bytes()); // partition_name_len
    d.extend_from_slice(&0u32.to_be_bytes()); // salt_len
    d.extend_from_slice(&0u32.to_be_bytes()); // digest_len
    d.extend_from_slice(&0u32.to_be_bytes()); // flags
    d.extend_from_slice(name);

    // Aux block: public key (empty) + descriptors
    let aux = d.clone();
    // Header
    let mut hdr = Vec::new();
    hdr.extend_from_slice(AVB_MAGIC);
    hdr.extend_from_slice(&1u32.to_be_bytes()); // major
    hdr.extend_from_slice(&2u32.to_be_bytes()); // minor
    hdr.extend_from_slice(&0u64.to_be_bytes()); // auth size
    hdr.extend_from_slice(&(aux.len() as u64).to_be_bytes()); // aux size
    hdr.extend_from_slice(&0u32.to_be_bytes()); // algorithm
    hdr.extend_from_slice(&0u64.to_be_bytes()); // hash off
    hdr.extend_from_slice(&0u64.to_be_bytes()); // hash size
    hdr.extend_from_slice(&0u64.to_be_bytes()); // sig off
    hdr.extend_from_slice(&0u64.to_be_bytes()); // sig size
    hdr.extend_from_slice(&0u64.to_be_bytes()); // pk off
    hdr.extend_from_slice(&0u64.to_be_bytes()); // pk size
    hdr.extend_from_slice(&0u64.to_be_bytes()); // pk meta off
    hdr.extend_from_slice(&0u64.to_be_bytes()); // pk meta size
    hdr.extend_from_slice(&0u64.to_be_bytes()); // descriptors off = 0 (relative to aux block)
    hdr.extend_from_slice(&(aux.len() as u64).to_be_bytes());
    hdr.extend_from_slice(&0u64.to_be_bytes()); // rollback
    hdr.extend_from_slice(&0u32.to_be_bytes()); // flags
    hdr.extend_from_slice(&0u32.to_be_bytes()); // rollback loc
    let mut rel = b"test release".to_vec();
    rel.resize(48, 0);
    hdr.extend_from_slice(&rel);

    let mut out = hdr;
    out.extend_from_slice(&aux);
    out
}

#[test]
fn parse_and_info() {
    let data = make_test_vbmeta();
    let v = VbmetaImage::parse(&data).unwrap();
    assert_eq!(v.header.required_libavb_version_major, 1);
    assert_eq!(v.header.required_libavb_version_minor, 2);
    assert_eq!(v.descriptors.len(), 1);
    assert_eq!(v.descriptors[0].tag, DescriptorTag::Hash);
    assert_eq!(v.descriptors[0].partition_name.as_deref(), Some("boot"));
    let info = v.info();
    assert!(info.contains("Hash: boot"));
    assert!(info.contains("Verification disabled: false"));
}

#[test]
fn disable_verification_sets_flags() {
    let data = make_test_vbmeta();
    let v = VbmetaImage::parse(&data).unwrap();
    let patched = v.disable_verification(FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED);
    let v2 = VbmetaImage::parse(&patched).unwrap();
    assert_eq!(v2.header.flags, 3);
    assert_eq!(v2.descriptors.len(), 1); // descriptors preserved
    let info = v2.info();
    assert!(info.contains("Hashtree disabled: true"));
    assert!(info.contains("Verification disabled: true"));
}

#[test]
fn strip_descriptors_empties_them() {
    let data = make_test_vbmeta();
    let v = VbmetaImage::parse(&data).unwrap();
    let stripped = v.strip_descriptors(3);
    let v2 = VbmetaImage::parse(&stripped).unwrap();
    assert_eq!(v2.header.flags, 3);
    assert!(v2.descriptors.is_empty());
}

#[test]
fn rejects_bad_magic_and_short() {
    let mut bad = vec![0u8; AVB_HEADER_SIZE];
    bad[0..4].copy_from_slice(b"AVB1");
    assert!(matches!(
        VbmetaImage::parse(&bad).unwrap_err(),
        AvbError::BadMagic
    ));
    assert!(matches!(
        VbmetaImage::parse(&[0u8; 16]).unwrap_err(),
        AvbError::TooSmall
    ));
}
