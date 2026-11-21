use super::{DeviceProfile, PAGE};

// =============================================================================
// GPT image generation
// =============================================================================

/// Image window size: enough to cover LBA0.. primary GPT + partition entries (32KB reads suffice, with margin).
const IMAGE_LEN: usize = 1024 * 1024;

const GPT_ENTRIES: u32 = 128;
const GPT_ENTRY_SIZE: u32 = 128;
/// Type GUID for every partition (Linux filesystem data, a fixed value suffices)
const TYPE_GUID: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Generate initial content for each partition (stored sparsely by 4 KiB page).
///
/// Only partitions "with content" occupy pages: the blank area outside the GPT reads back as 0x00 and does not waste memory.
pub(super) fn build_pages(profile: &DeviceProfile) -> std::collections::HashMap<u64, Vec<u8>> {
    use std::collections::HashMap;
    let mut pages: HashMap<u64, Vec<u8>> = HashMap::new();

    let mut put = |addr: u64, data: &[u8]| {
        let mut pos = addr;
        let end = addr + data.len() as u64;
        let mut src = 0usize;
        while pos < end {
            let page = pos / PAGE;
            let off = (pos % PAGE) as usize;
            let take = ((PAGE - off as u64).min(end - pos)) as usize;
            let buf = pages
                .entry(page)
                .or_insert_with(|| vec![0u8; PAGE as usize]);
            buf[off..off + take].copy_from_slice(&data[src..src + take]);
            pos += take as u64;
            src += take;
        }
    };

    for (name, start_lba, size) in &profile.partitions {
        let addr = start_lba * 512;
        match name.as_str() {
            // seccfg: real V4 structure (28-byte header + 32-byte hash), for unlock tests
            "seccfg" => {
                if profile.seccfg_v3 {
                    // V3: AND_SECCFG_v header + software SEJ encrypted section, end-to-end verification of unlock across all versions
                    let img = crate::security::seccfg::v3::synthetic_v3_image(
                        profile.seccfg_lock_state == 1,
                    );
                    put(addr, &img);
                } else {
                    let mut hdr = [0u8; 28];
                    hdr[0..4].copy_from_slice(&0x4D4D_4D4Du32.to_le_bytes());
                    hdr[4..8].copy_from_slice(&4u32.to_le_bytes());
                    hdr[8..12].copy_from_slice(&0x3Cu32.to_le_bytes());
                    hdr[12..16].copy_from_slice(&profile.seccfg_lock_state.to_le_bytes());
                    hdr[16..20].copy_from_slice(&profile.seccfg_dm_verity.to_le_bytes());
                    hdr[20..24].copy_from_slice(&0u32.to_le_bytes()); // sboot_runtime
                    hdr[24..28].copy_from_slice(&0x4545_4545u32.to_le_bytes());
                    let digest = crate::sha::Sha256::digest(hdr);
                    let mut img = hdr.to_vec();
                    img.extend_from_slice(&digest);
                    put(addr, &img);
                }
            }
            // vbmeta / vbmeta_system_*: valid AVB image (flags=0, includes boot descriptor)
            n if n.starts_with("vbmeta") => {
                let img = crate::security::avb::synthetic_vbmeta(0, &["boot"]);
                put(addr, &img);
            }
            // misc: boot_control structure ("BOOT" at offset 0x2000 + active slot)
            "misc" => {
                let mut img = vec![0u8; 0x3000];
                if profile.bootctrl {
                    img[0x2000..0x2004].copy_from_slice(b"BOOT");
                    let slot = match profile.slot {
                        Some('b') => 1u32,
                        _ => 0u32,
                    };
                    img[0x2008..0x200C].copy_from_slice(&slot.to_le_bytes());
                }
                put(addr, &img);
            }
            // Other partitions: write recognizable initial content (first 64 KiB) to verify read/write round-trips
            _ => {
                let len = (*size).min(64 * 1024) as usize;
                let mut img = vec![0u8; len];
                let seed = name.bytes().fold(0u8, |a, b| a.wrapping_add(b));
                for (i, b) in img.iter_mut().enumerate() {
                    *b = seed.wrapping_add(i as u8);
                }
                put(addr, &img);
            }
        }
    }
    pages
}

pub(super) fn build_image(profile: &DeviceProfile) -> Vec<u8> {
    let mut img = vec![0u8; IMAGE_LEN];
    // LBA0: protective MBR
    img[0x1BE] = 0x00; // not bootable
    img[0x1C2] = 0xEE; // type
    img[0x1C6..0x1CA].copy_from_slice(&1u32.to_le_bytes());
    let total_lbas = (profile.storage.user / 512).max(0x100000);
    img[0x1CA..0x1CE]
        .copy_from_slice(&((total_lbas - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    img[0x1FE] = 0x55;
    img[0x1FF] = 0xAA;

    // Partition entry array (starting at LBA2, 128*128)
    let entries_off = 1024usize;
    for (i, (name, start, size)) in profile.partitions.iter().enumerate() {
        let e = entries_off + i * GPT_ENTRY_SIZE as usize;
        img[e..e + 16].copy_from_slice(&TYPE_GUID);
        let mut guid = [0u8; 16];
        guid[0] = (i + 1) as u8;
        guid[15] = 0x42;
        img[e + 16..e + 32].copy_from_slice(&guid);
        let first = *start;
        let last = first + size / 512 - 1;
        img[e + 32..e + 40].copy_from_slice(&first.to_le_bytes());
        img[e + 40..e + 48].copy_from_slice(&last.to_le_bytes());
        // Name: UTF-16LE, up to 36 characters
        for (j, ch) in name.encode_utf16().take(35).enumerate() {
            img[e + 56 + j * 2..e + 58 + j * 2].copy_from_slice(&ch.to_le_bytes());
        }
    }
    let entries_len = (GPT_ENTRIES * GPT_ENTRY_SIZE) as usize;
    let entries_crc = crc32(&img[entries_off..entries_off + entries_len]);

    // LBA1: primary GPT header
    let h = 512usize;
    img[h..h + 8].copy_from_slice(b"EFI PART");
    img[h + 8..h + 12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    img[h + 12..h + 16].copy_from_slice(&92u32.to_le_bytes());
    img[h + 24..h + 32].copy_from_slice(&1u64.to_le_bytes()); // MyLBA
    img[h + 32..h + 40].copy_from_slice(&(total_lbas - 1).to_le_bytes()); // AltLBA
    img[h + 40..h + 48].copy_from_slice(&34u64.to_le_bytes()); // FirstUsable
    img[h + 48..h + 56].copy_from_slice(&(total_lbas - 34).to_le_bytes()); // LastUsable
    img[h + 56..h + 72].copy_from_slice(&[0x11; 16]); // DiskGUID
    img[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes()); // PartEntryLBA
    img[h + 80..h + 84].copy_from_slice(&GPT_ENTRIES.to_le_bytes());
    img[h + 84..h + 88].copy_from_slice(&GPT_ENTRY_SIZE.to_le_bytes());
    img[h + 88..h + 92].copy_from_slice(&entries_crc.to_le_bytes());
    let hdr_crc = {
        let mut tmp = img[h..h + 92].to_vec();
        tmp[16..20].fill(0);
        crc32(&tmp)
    };
    img[h + 16..h + 20].copy_from_slice(&hdr_crc.to_le_bytes());
    img
}
