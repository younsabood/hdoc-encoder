use thiserror::Error;

#[derive(Debug, Error)]
pub enum HdocError {
    #[error("invalid HyperDoc file: {0}")]
    InvalidFormat(String),
    #[error("unsupported HyperDoc version 0x{0:04x}")]
    UnsupportedVersion(u16),
    #[error("HyperDoc header checksum does not match")]
    ChecksumMismatch,
    #[error("truncated HyperDoc data")]
    Truncated,
    #[error("unknown page opcode 0x{0:02x}")]
    UnknownOpcode(u8),
    #[error("invalid UTF-8 in page text")]
    InvalidUtf8,
    #[error("requested value exceeds the HyperDoc format limit")]
    LimitExceeded,
}

#[derive(Debug, Error)]
pub enum EncoderError {
    #[error(transparent)]
    Format(#[from] HdocError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("a HyperDoc must contain at least one page")]
    EmptyDocument,
    #[error("invalid encoder option: {0}")]
    InvalidOption(&'static str),
    #[error("PDF import failed: {0}")]
    PdfIngest(String),
}
