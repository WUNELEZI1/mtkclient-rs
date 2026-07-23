/// 统一应用错误类型
///
/// 替代全项目散落的 `Result<..., String>`，区分不同错误种类。
/// 目前仅在新代码中使用，逐步替换旧代码。

#[derive(Debug)]
pub enum AppError {
    Usb(String),
    Protocol(String),
    Security(String),
    Io(std::io::Error),
    Parse(String),
    Config(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Usb(msg) => write!(f, "USB 错误: {}", msg),
            AppError::Protocol(msg) => write!(f, "协议错误: {}", msg),
            AppError::Security(msg) => write!(f, "安全错误: {}", msg),
            AppError::Io(err) => write!(f, "IO 错误: {}", err),
            AppError::Parse(msg) => write!(f, "解析错误: {}", msg),
            AppError::Config(msg) => write!(f, "配置错误: {}", msg),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err)
    }
}

impl From<&str> for AppError {
    fn from(msg: &str) -> Self {
        AppError::Protocol(msg.to_string())
    }
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::Protocol(msg)
    }
}