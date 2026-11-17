//! Standalone HyperDoc encoder. The decoder has its own implementation of the
//! file schema; this crate deliberately has no code or dependency in common
//! with `hdoc-decoder`.

mod asset_pool;
mod builder;
mod error;
mod profile;
#[cfg(feature = "pdf-ingest")]
mod svg_compiler;
#[cfg(feature = "pdf-ingest")]
mod vector_path;
mod writer;

#[cfg(feature = "pdf-ingest")]
mod pdf_ingest;

pub mod format;
pub mod opcodes;
pub mod search;
pub mod varint;

pub use asset_pool::AssetInput;
pub use builder::HdocEncoderBuilder;
pub use error::{EncoderError, HdocError};
pub use opcodes::{GlyphOffset, PageCommand, PageDisplayList, SvgGlyphStyle, SvgGlyphUse};
pub use profile::CompressionProfile;
pub use search::SearchBlock;
pub use writer::{EncodingOutput, EncodingStats};

#[cfg(feature = "pdf-ingest")]
pub use pdf_ingest::{PdfImportOptions, PdfImportStats};
