use super::*;

#[test]
fn status_code_zero_is_success() {
    let kind = StatusCode::from_u32(0);
    assert_eq!(kind, StatusCode::Success);
    assert!(kind.is_success());
    assert!(XFlashError::from_status(0).is_none());
}

#[test]
fn status_code_known_errors() {
    assert_eq!(
        StatusCode::from_u32(StatusCode::CODE_UNSUPPORTED_COMMAND),
        StatusCode::UnsupportedCommand
    );
    assert_eq!(
        StatusCode::from_u32(StatusCode::CODE_PROTOCOL_ERROR),
        StatusCode::ProtocolError
    );
}

#[test]
fn status_code_domain_classification() {
    // 0xC002____ → security domain
    assert_eq!(
        StatusCode::from_u32(0xC002_0001),
        StatusCode::SecurityError(0xC002_0001)
    );
    // 0xC004____ → device domain
    assert_eq!(
        StatusCode::from_u32(0xC004_0040),
        StatusCode::DeviceError(0xC004_0040)
    );
    // Codes without the 0xC prefix → unknown
    assert_eq!(
        StatusCode::from_u32(0x1234_5678),
        StatusCode::Unknown(0x1234_5678)
    );
}

#[test]
fn xflash_error_from_status_preserves_code() {
    let e = XFlashError::from_status(0xC001_0005).expect("应为错误");
    assert_eq!(e.code, 0xC001_0005);
    assert_eq!(e.kind, StatusCode::ProtocolError);
    // Display contains both the classification and the raw code
    let s = e.to_string();
    assert!(s.contains("协议错误"), "{}", s);
    assert!(s.contains("C0010005"), "{}", s);
}

#[test]
fn protocol_error_bad_magic_message() {
    let e = ProtocolError::BadMagic {
        got: 0xDEAD_BEEF,
        expected: 0xFEEE_EEEF,
    };
    let s = e.to_string();
    assert!(s.contains("DEADBEEF"), "{}", s);
    assert!(s.contains("FEEEEEEF"), "{}", s);
}

#[test]
fn structured_errors_convert_to_app_error() {
    let usb_err: AppError = UsbError::Timeout.into();
    assert!(matches!(usb_err, AppError::Usb(_)));
    let proto_err: AppError = ProtocolError::MessageFlood { count: 70 }.into();
    assert!(matches!(proto_err, AppError::Protocol(_)));
}

#[test]
fn session_error_display() {
    assert_eq!(SessionError::DaNotLoaded.to_string(), "DA 尚未加载");
}

#[test]
fn xml_error_maps_known_messages() {
    assert_eq!(
        XmlError::from_message(b"ERR!UNSUPPORTED\0").kind,
        XmlErrorKind::UnsupportedCmd
    );
    assert_eq!(
        XmlError::from_message(b"Invalid DA Version\0").kind,
        XmlErrorKind::AntiRollbackViolation
    );
    assert_eq!(
        XmlError::from_message(b"RPMB read failed\0").kind,
        XmlErrorKind::RpmbReadFailed
    );
    // No match → Other(msg), keeping the raw text
    let e = XmlError::from_message(b"some vendor text\0");
    assert_eq!(e.kind, XmlErrorKind::Other("some vendor text".into()));
    assert_eq!(e.message, "some vendor text");
    // Empty text → Unknown
    assert_eq!(XmlError::from_message(b"\0").kind, XmlErrorKind::Unknown);
}

#[test]
fn xml_error_from_kind_other_keeps_message() {
    let e = XmlError::from_kind(XmlErrorKind::Other("boom".into()));
    assert_eq!(e.message, "boom");
    // Non-Other variants use the classification's readable text
    let e2 = XmlError::from_kind(XmlErrorKind::UnsupportedCmd);
    assert_eq!(e2.message, "设备不支持该命令");
}

#[test]
fn xml_error_converts_to_app_error() {
    let app: AppError = XmlError::from_message(b"ERR!CANCEL").into();
    assert!(matches!(app, AppError::Protocol(_)));
}
