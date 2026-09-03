use thiserror::Error;

#[derive(Error, Debug)]
pub enum ArkxError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("Corrupted archive: {0}")]
    Corrupted(String),

    #[error("Password required or incorrect")]
    WrongPassword,

    #[error("Cancelled")]
    Cancelled,

    #[error("Backend error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, ArkxError>;
