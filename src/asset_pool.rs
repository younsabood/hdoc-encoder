use std::collections::HashMap;

use crate::EncoderError;

/// Already encoded media added to the container. Codec implementations stay
/// optional so text-only users do not pull image or font libraries.
#[derive(Debug, Clone)]
pub struct AssetInput {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

impl AssetInput {
    pub fn new(bytes: impl Into<Vec<u8>>, media_type: impl Into<String>) -> Self {
        Self {
            bytes: bytes.into(),
            media_type: media_type.into(),
            width: 0,
            height: 0,
        }
    }

    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StoredAsset {
    pub id: u32,
    pub input: AssetInput,
    pub hash: [u8; 32],
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AssetPool {
    entries: Vec<StoredAsset>,
    by_hash: HashMap<[u8; 32], u32>,
    duplicate_count: u64,
}

impl AssetPool {
    pub fn insert(&mut self, input: AssetInput) -> Result<u32, EncoderError> {
        if input.bytes.is_empty() {
            return Err(EncoderError::InvalidOption("asset bytes cannot be empty"));
        }
        if input.media_type.is_empty() {
            return Err(EncoderError::InvalidOption(
                "asset media type cannot be empty",
            ));
        }
        let hash = *blake3::hash(&input.bytes).as_bytes();
        if let Some(id) = self.by_hash.get(&hash).copied() {
            self.duplicate_count += 1;
            if let Some(existing) = self.entries.get_mut(id as usize) {
                if existing.input.width == 0 && input.width > 0 {
                    existing.input.width = input.width;
                }
                if existing.input.height == 0 && input.height > 0 {
                    existing.input.height = input.height;
                }
            }
            return Ok(id);
        }
        let id = u32::try_from(self.entries.len())
            .map_err(|_| EncoderError::InvalidOption("too many unique assets"))?;
        self.entries.push(StoredAsset { id, input, hash });
        self.by_hash.insert(hash, id);
        Ok(id)
    }

    pub fn entries(&self) -> &[StoredAsset] {
        &self.entries
    }

    pub fn duplicate_count(&self) -> u64 {
        self.duplicate_count
    }
}
