use std::collections::BTreeMap;

use crate::HdocError;

pub const FILE_MAGIC: [u8; 4] = *b"HDOC";
pub const INDEX_MAGIC: [u8; 4] = *b"HDIX";
pub const FORMAT_VERSION: u16 = 0x0100;
pub const HEADER_SIZE: usize = 32;
pub const PAGE_DIRECTORY_ENTRY_SIZE: usize = 16;
pub const END_MARKER: [u8; 16] = *b"HDOC-END-MARKER!";
pub const FLAG_ENCRYPTED: u16 = 1 << 0;
pub const FLAG_HAS_SEARCH_INDEX: u16 = 1 << 1;
pub const FLAG_ARCHIVAL: u16 = 1 << 2;

/// The fixed-size, 32-byte file header described by HyperDoc 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    pub version: u16,
    pub flags: u16,
    pub page_count: u32,
    pub asset_count: u32,
    pub footer_offset: u64,
    pub footer_size: u32,
}

impl FileHeader {
    pub fn new(page_count: u32, asset_count: u32, footer_offset: u64, footer_size: u32) -> Self {
        Self {
            version: FORMAT_VERSION,
            flags: FLAG_HAS_SEARCH_INDEX,
            page_count,
            asset_count,
            footer_offset,
            footer_size,
        }
    }

    pub fn encode(self) -> [u8; HEADER_SIZE] {
        let mut output = [0u8; HEADER_SIZE];
        output[0..4].copy_from_slice(&FILE_MAGIC);
        output[4..6].copy_from_slice(&self.version.to_le_bytes());
        output[6..8].copy_from_slice(&self.flags.to_le_bytes());
        output[8..12].copy_from_slice(&self.page_count.to_le_bytes());
        output[12..16].copy_from_slice(&self.asset_count.to_le_bytes());
        output[16..24].copy_from_slice(&self.footer_offset.to_le_bytes());
        output[24..28].copy_from_slice(&self.footer_size.to_le_bytes());
        let checksum = crc32fast::hash(&output[..28]);
        output[28..32].copy_from_slice(&checksum.to_le_bytes());
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, HdocError> {
        if input.len() < HEADER_SIZE {
            return Err(HdocError::Truncated);
        }
        if input[..4] != FILE_MAGIC {
            return Err(HdocError::InvalidFormat("missing HDOC magic".into()));
        }
        let expected = u32::from_le_bytes(input[28..32].try_into().expect("fixed header slice"));
        if crc32fast::hash(&input[..28]) != expected {
            return Err(HdocError::ChecksumMismatch);
        }
        let version = u16::from_le_bytes(input[4..6].try_into().expect("fixed header slice"));
        if version != FORMAT_VERSION {
            return Err(HdocError::UnsupportedVersion(version));
        }
        Ok(Self {
            version,
            flags: u16::from_le_bytes(input[6..8].try_into().expect("fixed header slice")),
            page_count: u32::from_le_bytes(input[8..12].try_into().expect("fixed header slice")),
            asset_count: u32::from_le_bytes(input[12..16].try_into().expect("fixed header slice")),
            footer_offset: u64::from_le_bytes(
                input[16..24].try_into().expect("fixed header slice"),
            ),
            footer_size: u32::from_le_bytes(input[24..28].try_into().expect("fixed header slice")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRecord {
    pub id: u32,
    pub offset: u64,
    pub length: u64,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRecord {
    pub offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub first_page: u32,
    pub page_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRecord {
    pub chunk_id: u32,
    pub offset_in_chunk: u32,
    pub width: u16,
    pub height: u16,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchBlockRecord {
    pub offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

/// Tables stored in the compressed central index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CentralIndex {
    pub assets: Vec<AssetRecord>,
    pub chunks: Vec<ChunkRecord>,
    pub pages: Vec<PageRecord>,
    pub metadata: BTreeMap<String, String>,
    pub search_block: SearchBlockRecord,
}

impl CentralIndex {
    pub fn encode(&self) -> Result<Vec<u8>, HdocError> {
        let mut out = Vec::new();
        out.extend_from_slice(&INDEX_MAGIC);
        put_u16(&mut out, FORMAT_VERSION);
        put_u16(&mut out, 0);
        put_count(&mut out, self.assets.len())?;
        put_count(&mut out, self.chunks.len())?;
        put_count(&mut out, self.pages.len())?;
        put_count(&mut out, self.metadata.len())?;
        put_u64(&mut out, self.search_block.offset);
        put_u32(&mut out, self.search_block.compressed_size);
        put_u32(&mut out, self.search_block.uncompressed_size);

        for (key, value) in &self.metadata {
            put_string(&mut out, key)?;
            put_string(&mut out, value)?;
        }
        for asset in &self.assets {
            put_u32(&mut out, asset.id);
            put_u64(&mut out, asset.offset);
            put_u64(&mut out, asset.length);
            put_string(&mut out, &asset.media_type)?;
            put_u32(&mut out, asset.width);
            put_u32(&mut out, asset.height);
            out.extend_from_slice(&asset.hash);
        }
        for chunk in &self.chunks {
            put_u64(&mut out, chunk.offset);
            put_u32(&mut out, chunk.compressed_size);
            put_u32(&mut out, chunk.uncompressed_size);
            put_u32(&mut out, chunk.first_page);
            put_u32(&mut out, chunk.page_count);
        }
        for page in &self.pages {
            put_u32(&mut out, page.chunk_id);
            put_u32(&mut out, page.offset_in_chunk);
            put_u16(&mut out, page.width);
            put_u16(&mut out, page.height);
            put_u32(&mut out, page.flags);
        }
        Ok(out)
    }

    pub fn decode(input: &[u8]) -> Result<Self, HdocError> {
        let mut cursor = Cursor::new(input);
        if cursor.take(4)? != INDEX_MAGIC {
            return Err(HdocError::InvalidFormat("missing HDIX index magic".into()));
        }
        let version = cursor.u16()?;
        if version != FORMAT_VERSION {
            return Err(HdocError::UnsupportedVersion(version));
        }
        let _flags = cursor.u16()?;
        let asset_count = cursor.u32()? as usize;
        let chunk_count = cursor.u32()? as usize;
        let page_count = cursor.u32()? as usize;
        let metadata_count = cursor.u32()? as usize;
        let search_block = SearchBlockRecord {
            offset: cursor.u64()?,
            compressed_size: cursor.u32()?,
            uncompressed_size: cursor.u32()?,
        };
        validate_count(metadata_count, cursor.remaining(), 4)?;
        validate_count(asset_count, cursor.remaining(), 60)?;
        validate_count(chunk_count, cursor.remaining(), 24)?;
        validate_count(page_count, cursor.remaining(), PAGE_DIRECTORY_ENTRY_SIZE)?;

        let mut metadata = BTreeMap::new();
        for _ in 0..metadata_count {
            metadata.insert(cursor.string()?, cursor.string()?);
        }
        let mut assets = Vec::with_capacity(asset_count);
        for _ in 0..asset_count {
            let id = cursor.u32()?;
            let offset = cursor.u64()?;
            let length = cursor.u64()?;
            let media_type = cursor.string()?;
            let width = cursor.u32()?;
            let height = cursor.u32()?;
            let hash = cursor.take(32)?.try_into().expect("fixed hash slice");
            assets.push(AssetRecord {
                id,
                offset,
                length,
                media_type,
                width,
                height,
                hash,
            });
        }
        let mut chunks = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            chunks.push(ChunkRecord {
                offset: cursor.u64()?,
                compressed_size: cursor.u32()?,
                uncompressed_size: cursor.u32()?,
                first_page: cursor.u32()?,
                page_count: cursor.u32()?,
            });
        }
        let mut pages = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            pages.push(PageRecord {
                chunk_id: cursor.u32()?,
                offset_in_chunk: cursor.u32()?,
                width: cursor.u16()?,
                height: cursor.u16()?,
                flags: cursor.u32()?,
            });
        }
        if cursor.remaining() != 0 {
            return Err(HdocError::InvalidFormat(
                "unexpected data after central index".into(),
            ));
        }
        Ok(Self {
            assets,
            chunks,
            pages,
            metadata,
            search_block,
        })
    }
}

fn put_count(output: &mut Vec<u8>, count: usize) -> Result<(), HdocError> {
    let count = u32::try_from(count).map_err(|_| HdocError::LimitExceeded)?;
    put_u32(output, count);
    Ok(())
}

fn put_string(output: &mut Vec<u8>, value: &str) -> Result<(), HdocError> {
    let len = u16::try_from(value.len()).map_err(|_| HdocError::LimitExceeded)?;
    put_u16(output, len);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn validate_count(count: usize, remaining: usize, minimum_size: usize) -> Result<(), HdocError> {
    let required = count
        .checked_mul(minimum_size)
        .ok_or(HdocError::LimitExceeded)?;
    if required > remaining {
        return Err(HdocError::Truncated);
    }
    Ok(())
}

struct Cursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.position)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], HdocError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(HdocError::LimitExceeded)?;
        let slice = self
            .input
            .get(self.position..end)
            .ok_or(HdocError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, HdocError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("fixed integer slice"),
        ))
    }

    fn u32(&mut self) -> Result<u32, HdocError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("fixed integer slice"),
        ))
    }

    fn u64(&mut self) -> Result<u64, HdocError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("fixed integer slice"),
        ))
    }

    fn string(&mut self) -> Result<String, HdocError> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| HdocError::InvalidUtf8)
    }
}
