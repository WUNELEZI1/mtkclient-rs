//! Android Virtual A/B COW (Copy-on-Write) format parser
//!
//! Supports two COW formats:
//! 1. AOSP libsnapshot COW v2/v3 (Android 12+, magic = 0xb5f0e642 / 0x94f09377)
//! 2. Linux Kernel dm-snapshot persistent (Android 11 VAB, magic = "SnAp" = 0x70416E53)
//!
//! Reference sources:
//! - AOSP: system/core/fs_mgr/libsnapshot/cow_format.h
//! - Linux Kernel: drivers/md/dm-snap-persistent.c

// ============================================================================
// Format detection
// ============================================================================

// ============================================================================
// dm-snapshot persistent format
// ============================================================================

// ============================================================================
// AOSP COW v2/v3 format (original implementation retained)
// ============================================================================

// ============================================================================
// Unified merge entry point
// ============================================================================
