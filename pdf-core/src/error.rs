use thiserror::Error;

#[derive(Debug, Error)]
pub enum PdfError {
    #[error("unexpected end of input at offset {0}")]
    UnexpectedEof(usize),
    #[error("unexpected byte {byte:#04x} at offset {offset}")]
    UnexpectedByte { byte: u8, offset: usize },
    #[error("invalid object at offset {0}: {1}")]
    InvalidObject(usize, String),
    #[error("xref table not found or invalid: {0}")]
    InvalidXref(String),
    #[error("object {0} {1} not found")]
    ObjectNotFound(u32, u16),
    #[error("missing required key /{0} in dictionary")]
    MissingKey(String),
    #[error("unexpected type: expected {0}")]
    UnexpectedType(&'static str),
    #[error("unsupported filter: {0}")]
    UnsupportedFilter(String),
    #[error("stream decode error: {0}")]
    DecodeError(String),
    #[error("encrypted PDF documents are not supported (/Encrypt present in trailer)")]
    Encrypted,
}

pub type Result<T> = std::result::Result<T, PdfError>;
