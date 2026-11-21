use super::super::types::LP_PARTITION_ATTR_SLOT_SUFFIXED;
use super::*;

const DEFAULT_HEADER_OFFSET: usize = 4096;
const HEADER_SIZE: u32 = 256;
const DATA_LEN: usize = 0x2000;

const PARTITION_ENTRY_SIZE: u32 = 52;
const EXTENT_ENTRY_SIZE: u32 = 24;
const GROUP_ENTRY_SIZE: u32 = 48;

// Table offsets are relative to tables_base = header_offset + header_size.
const PARTITIONS_TABLE_OFFSET: u32 = 0;
const EXTENTS_TABLE_OFFSET: u32 = 256;
const GROUPS_TABLE_OFFSET: u32 = 512;
const BLOCK_DEVICES_TABLE_OFFSET: u32 = 768;

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_name(buf: &mut [u8], off: usize, name: &str) {
    buf[off..off + name.len()].copy_from_slice(name.as_bytes());
}

fn write_table_descriptor(
    data: &mut [u8],
    off: usize,
    offset: u32,
    num_entries: u32,
    entry_size: u32,
) {
    put_u32(data, off, offset);
    put_u32(data, off + 4, num_entries);
    put_u32(data, off + 8, entry_size);
}

// find_geometry walks backwards from header_offset in 512-byte steps,
// starting at header_offset - 1. The second candidate is header_offset - 513;
// use it so the geometry block never overlaps the header itself.
fn write_geometry(data: &mut [u8], header_offset: usize, block_size: u32) {
    let off = header_offset - 513;
    put_u32(data, off, GEOMETRY_MAGIC);
    put_u32(data, off + 4, 4096); // struct_size
    put_u32(data, off + 40, 0); // metadata_max_size
    put_u32(data, off + 44, 0); // metadata_slot_count
    put_u32(data, off + 48, block_size); // some vendors store logical_block_size here
}

// (name, attributes, first_extent_index, num_extents)
type PartitionSpec<'a> = (&'a str, u32, u32, u32);
// (num_sectors, target_type, target_data, target_source)
type ExtentSpec = (u64, u32, u64, u32);

fn build_fixture(
    header_offset: usize,
    geometry_block_size: Option<u32>,
    partitions: &[PartitionSpec<'_>],
    extents: &[ExtentSpec],
    group_count: u32,
) -> Vec<u8> {
    let mut data = vec![0u8; DATA_LEN];

    if let Some(block_size) = geometry_block_size {
        write_geometry(&mut data, header_offset, block_size);
    }

    put_u32(&mut data, header_offset, HEADER_MAGIC);
    put_u16(&mut data, header_offset + 4, 10); // major_version
    put_u16(&mut data, header_offset + 6, 0); // minor_version
    put_u32(&mut data, header_offset + 8, HEADER_SIZE);
    write_table_descriptor(
        &mut data,
        header_offset + 80,
        PARTITIONS_TABLE_OFFSET,
        partitions.len() as u32,
        PARTITION_ENTRY_SIZE,
    );
    write_table_descriptor(
        &mut data,
        header_offset + 92,
        EXTENTS_TABLE_OFFSET,
        extents.len() as u32,
        EXTENT_ENTRY_SIZE,
    );
    write_table_descriptor(
        &mut data,
        header_offset + 104,
        GROUPS_TABLE_OFFSET,
        group_count,
        GROUP_ENTRY_SIZE,
    );
    write_table_descriptor(
        &mut data,
        header_offset + 116,
        BLOCK_DEVICES_TABLE_OFFSET,
        0,
        0,
    );

    let tables_base = header_offset + HEADER_SIZE as usize;
    for (i, (name, attributes, first_extent, num_extents)) in partitions.iter().enumerate() {
        let eo = tables_base + PARTITIONS_TABLE_OFFSET as usize + i * PARTITION_ENTRY_SIZE as usize;
        put_name(&mut data, eo, name);
        put_u32(&mut data, eo + 36, *attributes);
        put_u32(&mut data, eo + 40, *first_extent);
        put_u32(&mut data, eo + 44, *num_extents);
        put_u32(&mut data, eo + 48, 0); // group_index
    }
    for (i, (num_sectors, target_type, target_data, target_source)) in extents.iter().enumerate() {
        let eo = tables_base + EXTENTS_TABLE_OFFSET as usize + i * EXTENT_ENTRY_SIZE as usize;
        put_u64(&mut data, eo, *num_sectors);
        put_u32(&mut data, eo + 8, *target_type);
        put_u64(&mut data, eo + 12, *target_data);
        put_u32(&mut data, eo + 20, *target_source);
    }

    data
}

fn sample_partitions() -> Vec<PartitionSpec<'static>> {
    vec![
        ("system", 0, 0, 2),
        ("vendor_a", LP_PARTITION_ATTR_SLOT_SUFFIXED, 2, 1),
    ]
}

fn sample_extents() -> Vec<ExtentSpec> {
    vec![(100, 0, 1000, 0), (200, 0, 2000, 0), (50, 0, 3000, 0)]
}

fn sample_metadata() -> SuperMetadata {
    let data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(4096),
        &sample_partitions(),
        &sample_extents(),
        1,
    );
    SuperMetadata::parse(&data).expect("sample metadata must parse")
}

#[test]
fn parse_splits_all_tables() {
    let metadata = sample_metadata();

    assert_eq!(metadata.block_size, 4096);
    assert_eq!(metadata.partitions.len(), 2);
    assert_eq!(metadata.partitions[0].name, "system");
    assert_eq!(metadata.partitions[0].first_extent_index, 0);
    assert_eq!(metadata.partitions[0].num_extents, 2);
    assert_eq!(metadata.partitions[1].name, "vendor_a");
    assert!(metadata.partitions[1].is_slot_suffixed());

    assert_eq!(metadata.extents.len(), 3);
    assert_eq!(metadata.extents[0].num_sectors, 100);
    assert_eq!(metadata.extents[0].target_data, 1000);
    assert_eq!(metadata.extents[1].num_sectors, 200);
    assert_eq!(metadata.extents[2].num_sectors, 50);
    assert_eq!(metadata.extents[2].target_data, 3000);
}

#[test]
fn parse_empty_tables() {
    let data = build_fixture(DEFAULT_HEADER_OFFSET, Some(4096), &[], &[], 0);
    let metadata = SuperMetadata::parse(&data).expect("empty tables must parse");

    assert_eq!(metadata.block_size, 4096);
    assert!(metadata.partitions.is_empty());
    assert!(metadata.extents.is_empty());
}

#[test]
fn parse_without_geometry_uses_default_block_size() {
    let data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        None,
        &sample_partitions(),
        &sample_extents(),
        0,
    );
    let metadata = SuperMetadata::parse(&data).expect("metadata without geometry must parse");

    assert_eq!(metadata.block_size, 512);
    assert_eq!(metadata.partitions.len(), 2);
    assert_eq!(metadata.extents.len(), 3);
}

#[test]
fn parse_missing_header_magic_returns_err() {
    let data = vec![0u8; DATA_LEN];
    let err = SuperMetadata::parse(&data).expect_err("missing header magic must fail");
    assert!(err.contains("LP_METADATA_HEADER_MAGIC"), "{}", err);
}

#[test]
fn parse_unaligned_header_magic_is_found_by_byte_scan() {
    // 4000 is not a multiple of 512, so the aligned scan misses it and the
    // byte-by-byte fallback has to locate the header.
    let data = build_fixture(4000, Some(4096), &[], &[], 0);
    let metadata = SuperMetadata::parse(&data).expect("unaligned header must still parse");

    assert_eq!(metadata.block_size, 4096);
    assert!(metadata.partitions.is_empty());
}

#[test]
fn parse_partition_entry_size_too_small_returns_err() {
    let mut data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(4096),
        &sample_partitions(),
        &sample_extents(),
        0,
    );
    // Partition entry_size field: header_offset + 80 + 8.
    put_u32(&mut data, DEFAULT_HEADER_OFFSET + 88, 47);
    let err = SuperMetadata::parse(&data).expect_err("undersized partition entries must fail");
    assert!(err.contains("entry_size"), "{}", err);
}

#[test]
fn parse_extent_entry_size_too_small_returns_err() {
    let mut data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(4096),
        &sample_partitions(),
        &sample_extents(),
        0,
    );
    // Extent entry_size field: header_offset + 92 + 8.
    put_u32(&mut data, DEFAULT_HEADER_OFFSET + 100, 23);
    let err = SuperMetadata::parse(&data).expect_err("undersized extent entries must fail");
    assert!(err.contains("entry_size"), "{}", err);
}

#[test]
fn parse_out_of_bounds_table_offsets_yield_empty_tables() {
    let mut data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(4096),
        &sample_partitions(),
        &sample_extents(),
        0,
    );
    // Point both tables far past the end of the buffer; the per-entry
    // bounds check must stop parsing instead of reading out of range.
    put_u32(&mut data, DEFAULT_HEADER_OFFSET + 80, 0x0001_0000);
    put_u32(&mut data, DEFAULT_HEADER_OFFSET + 92, 0x0001_0000);
    let metadata = SuperMetadata::parse(&data).expect("out-of-bounds tables must not panic");

    assert!(metadata.partitions.is_empty());
    assert!(metadata.extents.is_empty());
}

#[test]
fn parse_num_entries_larger_than_buffer_does_not_panic() {
    let mut data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(4096),
        &sample_partitions(),
        &sample_extents(),
        0,
    );
    // Claim more partitions than the buffer can hold; parsing must stop
    // at the first out-of-range entry instead of panicking.
    put_u32(&mut data, DEFAULT_HEADER_OFFSET + 84, 1000);
    let metadata = SuperMetadata::parse(&data).expect("oversized entry count must not panic");

    let max_entries =
        (DATA_LEN - (DEFAULT_HEADER_OFFSET + HEADER_SIZE as usize)) / PARTITION_ENTRY_SIZE as usize;
    assert!(metadata.partitions.len() >= 2);
    assert!(metadata.partitions.len() <= max_entries);
}

#[test]
fn find_partition_smart_exact_match() {
    let metadata = sample_metadata();
    // "system" spans extents 0 and 1: start sector 1000, 300 sectors total.
    assert_eq!(
        metadata.find_partition_smart("system"),
        Some(("system".to_string(), 1000 * 4096, 300 * 4096))
    );
}

#[test]
fn find_partition_smart_a_suffix_match() {
    let metadata = sample_metadata();
    // No exact "vendor"; the _a slot variant must be returned.
    assert_eq!(
        metadata.find_partition_smart("vendor"),
        Some(("vendor_a".to_string(), 3000 * 4096, 50 * 4096))
    );
}

#[test]
fn find_partition_smart_b_suffix_match() {
    let data = build_fixture(
        DEFAULT_HEADER_OFFSET,
        Some(512),
        &[("boot_b", 0, 0, 1)],
        &[(10, 0, 20, 0)],
        0,
    );
    let metadata = SuperMetadata::parse(&data).expect("metadata must parse");
    assert_eq!(
        metadata.find_partition_smart("boot"),
        Some(("boot_b".to_string(), 20 * 512, 10 * 512))
    );
}

#[test]
fn find_partition_smart_no_match() {
    let metadata = sample_metadata();
    assert_eq!(metadata.find_partition_smart("missing"), None);
}
