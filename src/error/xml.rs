use std::fmt;

// =============================================================================
// XML (V6) DA protocol errors
// =============================================================================

/// Error classification returned by the XML/V6 DA protocol (mirrors penumbra `XmlErrorKind`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlErrorKind {
    /// Unknown/unclassified
    Unknown,
    /// Device does not support the command (`ERR!UNSUPPORTED`)
    UnsupportedCmd,
    /// Command cancelled (`ERR!CANCEL`)
    Cancel,
    /// DA anti-rollback check failed
    AntiRollbackViolation,
    /// Expected `CMD:DOWNLOAD-FILE` but got another command
    ExpectedCmdDownloadFile,
    /// Expected `CMD:UPLOAD-FILE` but got another command
    ExpectedCmdUploadFile,
    /// Expected `CMD:PROGRESS-REPORT` but got another command
    ExpectedCmdProgressReport,
    /// Expected `CMD:FILE-SYS-OPERATION` but got another command
    ExpectedFileSysOp,
    /// DA SLA signature rejected
    SlaSignatureRejected,
    /// Unknown path separator
    UnknownPathSep,
    /// Other error carrying raw text
    Other(String),
    /// Invalid KDF output key length (Extensions)
    InvalidKeyDeriveLength,
    /// Invalid KDF label/salt length (Extensions)
    InvalidLabelOrSaltLength,
    /// SEJ AES data length exceeds the limit (Extensions)
    SejAesLengthExceeded,
    /// Unknown storage type; cannot perform RPMB operations
    StorageUnknown,
    /// Invalid RPMB key format
    InvalidRpmbKey,
    /// RPMB partition not initialized
    RpmbNotInitialized,
    /// RPMB initialization failed
    RpmbInitFailed,
    /// RPMB read failed
    RpmbReadFailed,
    /// RPMB write failed
    RpmbWriteFailed,
}

impl fmt::Display for XmlErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            XmlErrorKind::Unknown => "未知错误",
            XmlErrorKind::UnsupportedCmd => "设备不支持该命令",
            XmlErrorKind::Cancel => "命令被取消",
            XmlErrorKind::AntiRollbackViolation => "DA 反回滚校验失败",
            XmlErrorKind::ExpectedCmdDownloadFile => "期望 CMD:DOWNLOAD-FILE",
            XmlErrorKind::ExpectedCmdUploadFile => "期望 CMD:UPLOAD-FILE",
            XmlErrorKind::ExpectedCmdProgressReport => "期望 CMD:PROGRESS-REPORT",
            XmlErrorKind::ExpectedFileSysOp => "期望 CMD:FILE-SYS-OPERATION",
            XmlErrorKind::SlaSignatureRejected => "DA SLA 签名被拒绝",
            XmlErrorKind::UnknownPathSep => "未知路径分隔符",
            XmlErrorKind::Other(msg) => msg.as_str(),
            XmlErrorKind::InvalidKeyDeriveLength => "KDF 输出密钥长度非法",
            XmlErrorKind::InvalidLabelOrSaltLength => "KDF label/salt 长度非法",
            XmlErrorKind::SejAesLengthExceeded => "SEJ AES 数据长度超限",
            XmlErrorKind::StorageUnknown => "存储类型未知，无法执行 RPMB 操作",
            XmlErrorKind::InvalidRpmbKey => "RPMB key 格式/长度非法",
            XmlErrorKind::RpmbNotInitialized => "RPMB 分区未初始化",
            XmlErrorKind::RpmbInitFailed => "RPMB 初始化失败",
            XmlErrorKind::RpmbReadFailed => "RPMB 读取失败",
            XmlErrorKind::RpmbWriteFailed => "RPMB 写入失败",
        };
        write!(f, "{}", s)
    }
}

/// XML/V6 DA protocol error carrying the classification and the raw device text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    /// Raw error text from the device (trailing `\0` removed)
    pub message: String,
    /// Error classification
    pub kind: XmlErrorKind,
}

impl XmlError {
    /// Build from a classification (`Other` keeps its inner text)
    pub fn from_kind(kind: XmlErrorKind) -> Self {
        let message = match &kind {
            XmlErrorKind::Other(msg) => msg.clone(),
            other => other.to_string(),
        };
        XmlError { message, kind }
    }

    /// Parse device error text and map it to [`XmlErrorKind`]
    ///
    /// Mirrors penumbra `XmlError::from_message`: strip the trailing `\0`, then match the known
    /// error texts exactly; unmatched text becomes [`XmlErrorKind::Other`] (keeping it), and
    /// empty text becomes [`XmlErrorKind::Unknown`].
    pub fn from_message(resp: &[u8]) -> Self {
        let msg = String::from_utf8_lossy(resp);
        let msg = msg.trim_end_matches('\0');
        // Empty text (no reason from the device) maps to Unknown instead of degrading to Other("")
        if msg.is_empty() {
            return XmlError::from_kind(XmlErrorKind::Unknown);
        }
        let kind = match msg {
            "ERR!UNSUPPORTED" => XmlErrorKind::UnsupportedCmd,
            "ERR!CANCEL" => XmlErrorKind::Cancel,
            "Invalid DA Version" => XmlErrorKind::AntiRollbackViolation,
            "Server is not authenticated. Locked." => XmlErrorKind::SlaSignatureRejected,
            "Unknow path separator." => XmlErrorKind::UnknownPathSep,
            "Invalid key derive output length" => XmlErrorKind::InvalidKeyDeriveLength,
            "Invalid label or salt length" => XmlErrorKind::InvalidLabelOrSaltLength,
            "SEJ AES data length exceeds maximum allowed" => XmlErrorKind::SejAesLengthExceeded,
            "Storage type unknown, cannot initialize RPMB"
            | "Storage type unknown, cannot read RPMB"
            | "Storage type unknown, cannot write RPMB" => XmlErrorKind::StorageUnknown,
            "RPMB key must be 64 hex chars (32 bytes)" | "Invalid RPMB key format" => {
                XmlErrorKind::InvalidRpmbKey
            }
            "RPMB partition not initialized" => XmlErrorKind::RpmbNotInitialized,
            "RPMB initialization failed" => XmlErrorKind::RpmbInitFailed,
            "RPMB read failed" => XmlErrorKind::RpmbReadFailed,
            "RPMB write failed" => XmlErrorKind::RpmbWriteFailed,
            other => XmlErrorKind::Other(other.to_string()),
        };
        XmlError::from_kind(kind)
    }
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XML DA 错误: {}", self.message)
    }
}

impl std::error::Error for XmlError {}
