use std::collections::BTreeMap;

use crate::{asset_pool::AssetPool, AssetInput, CompressionProfile, EncoderError, PageDisplayList};

/// Input builder for a HyperDoc container.
#[derive(Debug, Clone)]
pub struct HdocEncoderBuilder {
    pub(crate) pages: Vec<PageDisplayList>,
    pub(crate) page_texts: Vec<String>,
    pub(crate) assets: AssetPool,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) profile: CompressionProfile,
    pub(crate) max_pages_per_chunk: usize,
    pub(crate) target_chunk_bytes: usize,
    pub(crate) include_search_index: bool,
}

impl Default for HdocEncoderBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl HdocEncoderBuilder {
    pub fn new() -> Self {
        Self {
            pages: Vec::new(),
            page_texts: Vec::new(),
            assets: AssetPool::default(),
            metadata: BTreeMap::new(),
            profile: CompressionProfile::Archival,
            max_pages_per_chunk: 64,
            target_chunk_bytes: 8 * 1024 * 1024,
            include_search_index: true,
        }
    }

    /// Add one page and return its zero-based page number.
    pub fn add_page(&mut self, page: PageDisplayList) -> Result<u32, EncoderError> {
        if page.width == 0 || page.height == 0 {
            return Err(EncoderError::InvalidOption(
                "page width and height must be non-zero",
            ));
        }
        let page_id = u32::try_from(self.pages.len())
            .map_err(|_| EncoderError::InvalidOption("too many pages"))?;
        self.pages.push(page);
        self.page_texts.push(String::new());
        Ok(page_id)
    }

    /// Add a page and its logical Unicode text separately from its drawing opcodes.
    pub fn add_page_with_text(
        &mut self,
        page: PageDisplayList,
        text: impl Into<String>,
    ) -> Result<u32, EncoderError> {
        let page_id = self.add_page(page)?;
        self.page_texts[page_id as usize] = text.into();
        Ok(page_id)
    }

    /// Attach searchable UTF-8 text to a page without putting it in its glyph opcodes.
    pub fn page_text(
        &mut self,
        page_id: u32,
        text: impl Into<String>,
    ) -> Result<&mut Self, EncoderError> {
        let Some(page_text) = self.page_texts.get_mut(page_id as usize) else {
            return Err(EncoderError::InvalidOption(
                "page text refers to an unknown page",
            ));
        };
        *page_text = text.into();
        Ok(self)
    }

    /// Add a media asset and return its deduplicated zero-based asset ID.
    pub fn add_asset(&mut self, asset: AssetInput) -> Result<u32, EncoderError> {
        self.assets.insert(asset)
    }

    pub fn metadata(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn profile(&mut self, profile: CompressionProfile) -> &mut Self {
        self.profile = profile;
        self
    }

    /// Set the maximum pages and target uncompressed bytes per solid chunk.
    /// An individual page over the byte target is placed in its own chunk.
    pub fn chunk_limits(
        &mut self,
        max_pages: usize,
        target_uncompressed_bytes: usize,
    ) -> Result<&mut Self, EncoderError> {
        if max_pages == 0 {
            return Err(EncoderError::InvalidOption(
                "max pages per chunk must be greater than zero",
            ));
        }
        if target_uncompressed_bytes == 0 {
            return Err(EncoderError::InvalidOption(
                "chunk byte target must be greater than zero",
            ));
        }
        if target_uncompressed_bytes > 64 * 1024 * 1024 {
            return Err(EncoderError::InvalidOption(
                "chunk byte target cannot exceed 64 MiB",
            ));
        }
        self.max_pages_per_chunk = max_pages;
        self.target_chunk_bytes = target_uncompressed_bytes;
        Ok(self)
    }

    pub fn include_search_index(&mut self, enabled: bool) -> &mut Self {
        self.include_search_index = enabled;
        self
    }

    pub fn encode(&self) -> Result<crate::EncodingOutput, EncoderError> {
        crate::writer::encode(self)
    }

    pub fn write_to<W: std::io::Write>(
        &self,
        mut writer: W,
    ) -> Result<crate::EncodingStats, EncoderError> {
        let output = self.encode()?;
        writer.write_all(&output.bytes)?;
        Ok(output.stats)
    }

    pub fn write_to_path(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<crate::EncodingStats, EncoderError> {
        let output = self.encode()?;
        std::fs::write(path, &output.bytes)?;
        Ok(output.stats)
    }
}
