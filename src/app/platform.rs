/// Check the Windows version; Windows 10 or later is required
#[cfg(target_os = "windows")]
pub(crate) fn check_windows_version() -> Result<(), String> {
    #[repr(C)]
    struct OSVERSIONINFOEXW {
        dw_os_version_info_size: u32,
        dw_major_version: u32,
        dw_minor_version: u32,
        dw_build_number: u32,
        dw_platform_id: u32,
        sz_csd_version: [u16; 128],
        w_service_pack_major: u16,
        w_service_pack_minor: u16,
        w_suite_mask: u16,
        w_product_type: u8,
        w_reserved: u8,
    }

    unsafe extern "system" {
        fn RtlGetVersion(lp_version_information: *mut OSVERSIONINFOEXW) -> i32;
    }

    let mut info = OSVERSIONINFOEXW {
        dw_os_version_info_size: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
        dw_major_version: 0,
        dw_minor_version: 0,
        dw_build_number: 0,
        dw_platform_id: 0,
        sz_csd_version: [0; 128],
        w_service_pack_major: 0,
        w_service_pack_minor: 0,
        w_suite_mask: 0,
        w_product_type: 0,
        w_reserved: 0,
    };

    unsafe {
        RtlGetVersion(&mut info);
    }

    // Windows 10 = Major 10, Build >= 10240
    // Windows 11 = Major 10, Build >= 22000
    if info.dw_major_version < 10 {
        return Err(crate::i18n::tr(
            "os_unsupported",
            &[
                &info.dw_major_version.to_string(),
                &info.dw_minor_version.to_string(),
                &info.dw_build_number.to_string(),
            ],
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    pub(crate) fn SetConsoleOutputCP(wCodePage: u32) -> i32;
    pub(crate) fn SetConsoleCP(wCodePage: u32) -> i32;
}
