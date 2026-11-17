use std::io::Write;

use crate::{
    asset_pool::StoredAsset,
    builder::HdocEncoderBuilder,
    format::{
        AssetRecord, CentralIndex, ChunkRecord, FileHeader, PageRecord, SearchBlockRecord,
        END_MARKER, FLAG_ARCHIVAL, FLAG_HAS_SEARCH_INDEX, HEADER_SIZE,
    },
    opcodes::encode_page,
    search::SearchBlock,
    EncoderError,
};

const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
const MAX_SEARCH_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const VECTOR_PATH_MEDIA_PREFIX: &str = "application/vnd.hyperdoc.vector-path";
const VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE: &str =
    "application/vnd.hyperdoc.vector-path+zstd-dict;version=1";
const VECTOR_DICTIONARY_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-dictionary;version=1";
const VECTOR_DICTIONARY_MAX_SIZE: usize = 8 * 1024;
const VECTOR_DICTIONARY_COMPRESSION_LEVEL: i32 = 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodingStats {
    pub page_count: u32,
    pub unique_asset_count: u32,
    pub duplicate_asset_count: u64,
    /// Uncompressed page bytecode plus unique asset bytes supplied.
    pub input_payload_bytes: u64,
    pub output_bytes: u64,
    pub chunk_count: u32,
}

#[derive(Debug, Clone)]
pub struct EncodingOutput {
    pub bytes: Vec<u8>,
    pub stats: EncodingStats,
}

#[derive(Debug, Clone)]
struct PreparedAsset {
    id: u32,
    bytes: Vec<u8>,
    media_type: String,
    width: u32,
    height: u32,
    hash: [u8; 32],
}

pub(crate) fn encode(builder: &HdocEncoderBuilder) -> Result<EncodingOutput, EncoderError> {
    if builder.pages.is_empty() {
        return Err(EncoderError::EmptyDocument);
    }
    let prepared_assets = prepare_assets(builder.assets.entries())?;
    let mut output = vec![0u8; HEADER_SIZE];
    let mut assets = Vec::with_capacity(prepared_assets.len());
    let mut input_payload_bytes = 0u64;

    for item in &prepared_assets {
        let offset = u64::try_from(output.len())
            .map_err(|_| EncoderError::InvalidOption("file exceeds supported size"))?;
        let length = u64::try_from(item.bytes.len())
            .map_err(|_| EncoderError::InvalidOption("asset is too large"))?;
        output.extend_from_slice(&item.bytes);
        input_payload_bytes = input_payload_bytes.saturating_add(length);
        assets.push(AssetRecord {
            id: item.id,
            offset,
            length,
            media_type: item.media_type.clone(),
            width: item.width,
            height: item.height,
            hash: item.hash,
        });
    }

    let encoded_pages = builder
        .pages
        .iter()
        .map(encode_page)
        .collect::<Result<Vec<_>, _>>()?;
    for page in &encoded_pages {
        input_payload_bytes = input_payload_bytes.saturating_add(page.len() as u64);
    }
    let (chunks, page_directory) = write_chunks(builder, &encoded_pages, &mut output)?;
    let search_block = if builder.include_search_index {
        let raw_search = SearchBlock::from_page_texts(builder.page_texts.clone()).encode()?;
        if raw_search.len() > MAX_SEARCH_BLOCK_BYTES {
            return Err(EncoderError::InvalidOption(
                "uncompressed search block exceeds 64 MiB",
            ));
        }
        let compressed = zstd::bulk::compress(&raw_search, builder.profile.zstd_level())?;
        let offset = u64::try_from(output.len())
            .map_err(|_| EncoderError::InvalidOption("file exceeds supported size"))?;
        let compressed_size = u32::try_from(compressed.len())
            .map_err(|_| EncoderError::InvalidOption("search block exceeds 4 GiB"))?;
        let uncompressed_size = u32::try_from(raw_search.len())
            .map_err(|_| EncoderError::InvalidOption("search block exceeds 4 GiB"))?;
        output.extend_from_slice(&compressed);
        SearchBlockRecord {
            offset,
            compressed_size,
            uncompressed_size,
        }
    } else {
        SearchBlockRecord::default()
    };
    let mut metadata = builder.metadata.clone();
    metadata.insert("format_version".into(), "1.0".into());
    metadata.insert(
        "compression_profile".into(),
        builder.profile.as_metadata().into(),
    );
    let index = CentralIndex {
        assets,
        chunks,
        pages: page_directory,
        metadata,
        search_block,
    };
    let raw_index = index.encode()?;
    if raw_index.len() > MAX_INDEX_BYTES {
        return Err(EncoderError::InvalidOption(
            "uncompressed central index exceeds 128 MiB",
        ));
    }
    let compressed_index = zstd::bulk::compress(&raw_index, builder.profile.zstd_level())?;
    let footer_offset = u64::try_from(output.len())
        .map_err(|_| EncoderError::InvalidOption("file exceeds supported size"))?;
    let footer_size = u32::try_from(compressed_index.len())
        .map_err(|_| EncoderError::InvalidOption("central index exceeds 4 GiB"))?;
    output.extend_from_slice(&compressed_index);
    output.extend_from_slice(&END_MARKER);

    let asset_count = u32::try_from(prepared_assets.len())
        .map_err(|_| EncoderError::InvalidOption("too many assets"))?;
    let mut header = FileHeader::new(
        u32::try_from(builder.pages.len())
            .map_err(|_| EncoderError::InvalidOption("too many pages"))?,
        asset_count,
        footer_offset,
        footer_size,
    );
    header.flags = 0;
    if builder.include_search_index {
        header.flags |= FLAG_HAS_SEARCH_INDEX;
    }
    if builder.profile.is_archival() {
        header.flags |= FLAG_ARCHIVAL;
    }
    output[..HEADER_SIZE].copy_from_slice(&header.encode());

    let stats = EncodingStats {
        page_count: header.page_count,
        unique_asset_count: header.asset_count,
        duplicate_asset_count: builder.assets.duplicate_count(),
        input_payload_bytes,
        output_bytes: output.len() as u64,
        chunk_count: u32::try_from(index.chunks.len())
            .map_err(|_| EncoderError::InvalidOption("too many chunks"))?,
    };
    Ok(EncodingOutput {
        bytes: output,
        stats,
    })
}

fn prepare_assets(entries: &[StoredAsset]) -> Result<Vec<PreparedAsset>, EncoderError> {
    let mut prepared = entries
        .iter()
        .map(|asset| PreparedAsset {
            id: asset.id,
            bytes: asset.input.bytes.clone(),
            media_type: asset.input.media_type.clone(),
            width: asset.input.width,
            height: asset.input.height,
            hash: asset.hash,
        })
        .collect::<Vec<_>>();

    let path_indices = entries
        .iter()
        .enumerate()
        .filter_map(|(index, asset)| {
            asset
                .input
                .media_type
                .starts_with(VECTOR_PATH_MEDIA_PREFIX)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if path_indices.len() < 32 {
        return Ok(prepared);
    }

    let mut path_samples = Vec::with_capacity(path_indices.len());
    let mut original_path_bytes = 0usize;
    for index in &path_indices {
        let asset = &entries[*index];
        original_path_bytes = original_path_bytes.saturating_add(asset.input.bytes.len());
        let sample = if asset.input.media_type.contains("+zstd") {
            zstd::bulk::decompress(&asset.input.bytes, 16 * 1024 * 1024)?
        } else {
            asset.input.bytes.clone()
        };
        path_samples.push(sample);
    }

    let dictionary_size = VECTOR_DICTIONARY_MAX_SIZE.min(original_path_bytes / 8);
    if dictionary_size < 256 {
        return Ok(prepared);
    }
    let Ok(dictionary) = zstd::dict::from_samples(&path_samples, dictionary_size) else {
        return Ok(prepared);
    };
    if dictionary.is_empty() {
        return Ok(prepared);
    }

    let mut compressor =
        zstd::bulk::Compressor::with_dictionary(VECTOR_DICTIONARY_COMPRESSION_LEVEL, &dictionary)?;
    let mut compressed_paths = Vec::with_capacity(path_samples.len());
    let mut compressed_path_bytes = 0usize;
    for sample in &path_samples {
        let compressed = compressor.compress(sample)?;
        compressed_path_bytes = compressed_path_bytes.saturating_add(compressed.len());
        compressed_paths.push(compressed);
    }
    if compressed_path_bytes.saturating_add(dictionary.len()) >= original_path_bytes {
        return Ok(prepared);
    }

    for (index, compressed) in path_indices.iter().zip(compressed_paths) {
        prepared[*index].bytes = compressed;
        prepared[*index].media_type = VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE.into();
        prepared[*index].hash = *blake3::hash(&prepared[*index].bytes).as_bytes();
    }
    let dictionary_id = u32::try_from(prepared.len())
        .map_err(|_| EncoderError::InvalidOption("too many assets"))?;
    prepared.push(PreparedAsset {
        id: dictionary_id,
        hash: *blake3::hash(&dictionary).as_bytes(),
        bytes: dictionary,
        media_type: VECTOR_DICTIONARY_MEDIA_TYPE.into(),
        width: 0,
        height: 0,
    });
    Ok(prepared)
}

fn write_chunks(
    builder: &HdocEncoderBuilder,
    pages: &[Vec<u8>],
    output: &mut Vec<u8>,
) -> Result<(Vec<ChunkRecord>, Vec<PageRecord>), EncoderError> {
    let mut chunks = Vec::new();
    let mut directory = Vec::with_capacity(pages.len());
    let mut page_index = 0usize;
    while page_index < pages.len() {
        let first_page = page_index;
        let chunk_id = u32::try_from(chunks.len())
            .map_err(|_| EncoderError::InvalidOption("too many chunks"))?;
        let mut raw = Vec::new();
        let mut local_records = Vec::new();
        while page_index < pages.len() && local_records.len() < builder.max_pages_per_chunk {
            let page = &builder.pages[page_index];
            let encoded = &pages[page_index];
            if encoded.len() > MAX_CHUNK_BYTES {
                return Err(EncoderError::InvalidOption(
                    "one page exceeds the 64 MiB decompression limit",
                ));
            }
            if !local_records.is_empty()
                && raw.len().saturating_add(encoded.len()) > builder.target_chunk_bytes
            {
                break;
            }
            let offset = u32::try_from(raw.len())
                .map_err(|_| EncoderError::InvalidOption("uncompressed chunk exceeds 4 GiB"))?;
            raw.extend_from_slice(encoded);
            local_records.push(PageRecord {
                chunk_id,
                offset_in_chunk: offset,
                width: page.width,
                height: page.height,
                flags: page.flags,
            });
            page_index += 1;
        }
        let compressed = zstd::bulk::compress(&raw, builder.profile.zstd_level())?;
        let chunk_offset = u64::try_from(output.len())
            .map_err(|_| EncoderError::InvalidOption("file exceeds supported size"))?;
        let compressed_size = u32::try_from(compressed.len())
            .map_err(|_| EncoderError::InvalidOption("compressed chunk exceeds 4 GiB"))?;
        let uncompressed_size = u32::try_from(raw.len())
            .map_err(|_| EncoderError::InvalidOption("uncompressed chunk exceeds 4 GiB"))?;
        output.write_all(&compressed)?;
        chunks.push(ChunkRecord {
            offset: chunk_offset,
            compressed_size,
            uncompressed_size,
            first_page: u32::try_from(first_page)
                .map_err(|_| EncoderError::InvalidOption("too many pages"))?,
            page_count: u32::try_from(local_records.len())
                .map_err(|_| EncoderError::InvalidOption("too many pages in chunk"))?,
        });
        directory.extend(local_records);
    }
    Ok((chunks, directory))
}
