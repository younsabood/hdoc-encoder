#![cfg(feature = "pdf-ingest")]

use std::{collections::HashMap, path::PathBuf};

use hdoc_encoder::{
    format::{CentralIndex, FileHeader},
    opcodes::{decode_page, PageCommand},
    HdocEncoderBuilder, SearchBlock,
};

fn project_pdf(name: &str) -> Option<Vec<u8>> {
    let directory = std::env::var_os("HDOC_PDF_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
        });
    std::fs::read(directory.join(name)).ok()
}

fn encode_and_inspect(name: &str) {
    let Some(pdf) = project_pdf(name) else {
        eprintln!(
            "skipped {name}: set HDOC_PDF_TEST_DIR to a directory containing the sample PDFs"
        );
        return;
    };
    let mut builder = HdocEncoderBuilder::new();
    let imported = builder
        .import_pdf_bytes(&pdf)
        .expect("sample PDF converts to vector SVG pages");
    assert!(imported.page_count > 0, "{name} has pages");
    assert!(
        imported.svg_vector_bytes > 0,
        "{name} produced vector page payloads"
    );
    assert!(
        imported.avif_image_bytes > 0,
        "{name} converted embedded raster images to AVIF"
    );

    let encoded = builder.encode().expect("PDF pages serialize to HDOC");
    let reduction_percent =
        (pdf.len() as f64 - encoded.bytes.len() as f64) * 100.0 / pdf.len() as f64;
    println!(
        "{name}: PDF={} bytes, SVG template input={} bytes, PNG->AVIF={}, JPEG->AVIF={}, source raster={} bytes, AVIF={} bytes, unique assets={}, duplicates={}, HDOC={} bytes, reduction={reduction_percent:.1}%",
        pdf.len(),
        imported.svg_vector_bytes,
        imported.png_images_converted,
        imported.jpeg_images_converted,
        imported.source_image_bytes,
        imported.avif_image_bytes,
        encoded.stats.unique_asset_count,
        encoded.stats.duplicate_asset_count,
        encoded.bytes.len()
    );
    let header = FileHeader::decode(&encoded.bytes).expect("output has a valid 32-byte header");
    assert_eq!(header.page_count, imported.page_count);
    let footer_start = header.footer_offset as usize;
    let footer_end = footer_start + header.footer_size as usize;
    let raw_index =
        zstd::bulk::decompress(&encoded.bytes[footer_start..footer_end], 128 * 1024 * 1024)
            .unwrap();
    let index = CentralIndex::decode(&raw_index).unwrap();
    assert_eq!(index.pages.len() as u32, imported.page_count);
    let vector_path_assets = index
        .assets
        .iter()
        .filter(|asset| {
            asset
                .media_type
                .starts_with("application/vnd.hyperdoc.vector-path")
        })
        .collect::<Vec<_>>();
    assert!(
        !vector_path_assets.is_empty(),
        "repeated SVG glyph outlines are stored as deduplicated vector path assets"
    );
    assert!(
        index.assets.iter().all(|asset| {
            matches!(
                asset.media_type.as_str(),
                "image/avif" | "image/png" | "image/jpeg"
            ) || asset
                .media_type
                .starts_with("application/vnd.hyperdoc.vector-path")
                || asset.media_type == "application/vnd.hyperdoc.vector-dictionary;version=1"
        }),
        "asset pool contains deduplicated binary images and vector data"
    );
    let dictionary_asset = index
        .assets
        .iter()
        .find(|asset| asset.media_type == "application/vnd.hyperdoc.vector-dictionary;version=1")
        .map(|asset| &encoded.bytes[asset.offset as usize..(asset.offset + asset.length) as usize]);
    for asset in vector_path_assets {
        let start = asset.offset as usize;
        let end = start + asset.length as usize;
        let stored = &encoded.bytes[start..end];
        let decoded = if asset.media_type.contains("+zstd-dict") {
            let mut decompressor = zstd::bulk::Decompressor::with_dictionary(
                dictionary_asset.expect("dictionary-compressed paths include a dictionary asset"),
            )
            .unwrap();
            decompressor.decompress(stored, 16 * 1024 * 1024).unwrap()
        } else if asset.media_type.contains("+zstd") {
            zstd::bulk::decompress(stored, 16 * 1024 * 1024).unwrap()
        } else {
            stored.to_vec()
        };
        assert_eq!(
            &decoded[..4],
            b"HPTH",
            "vector paths use the binary path schema"
        );
    }
    assert_page_images_use_deduplicated_binary_assets(&encoded.bytes, &index, name);

    let search = &index.search_block;
    let raw_search = zstd::bulk::decompress(
        &encoded.bytes
            [search.offset as usize..search.offset as usize + search.compressed_size as usize],
        search.uncompressed_size as usize,
    )
    .unwrap();
    let search = SearchBlock::decode(&raw_search).unwrap();
    assert_eq!(search.page_texts.len() as u32, imported.page_count);

    let text_page_id = search
        .page_texts
        .iter()
        .position(|text| !text.trim().is_empty())
        .expect("PDF text extractor found searchable text");
    let page_record = &index.pages[text_page_id];
    let chunk_record = &index.chunks[page_record.chunk_id as usize];
    let raw_chunk = zstd::bulk::decompress(
        &encoded.bytes[chunk_record.offset as usize
            ..chunk_record.offset as usize + chunk_record.compressed_size as usize],
        chunk_record.uncompressed_size as usize,
    )
    .unwrap();
    let (page, _) = decode_page(
        &raw_chunk[page_record.offset_in_chunk as usize..],
        page_record.width,
        page_record.height,
        page_record.flags,
    )
    .unwrap();
    assert!(
        page.commands.iter().any(|command| matches!(
            command,
            PageCommand::DrawSvgTemplate { svg, glyphs, .. }
                if svg.contains("<svg")
                    && svg.contains("<!--HDOC_GLYPH-->")
                    && !glyphs.is_empty()
                    && !svg.contains("base64,")
        )),
        "{name} stores text as binary vector glyph paths and keeps raster images out of Base64"
    );
}

fn assert_page_images_use_deduplicated_binary_assets(
    bytes: &[u8],
    index: &CentralIndex,
    name: &str,
) {
    let mut decoded_chunks = HashMap::<u32, Vec<u8>>::new();

    for (page_id, page_record) in index.pages.iter().enumerate() {
        let chunk_record = &index.chunks[page_record.chunk_id as usize];
        let raw_chunk = decoded_chunks
            .entry(page_record.chunk_id)
            .or_insert_with(|| {
                zstd::bulk::decompress(
                    &bytes[chunk_record.offset as usize
                        ..chunk_record.offset as usize + chunk_record.compressed_size as usize],
                    chunk_record.uncompressed_size as usize,
                )
                .expect("generated page chunk decompresses")
            });
        let (page, _) = decode_page(
            &raw_chunk[page_record.offset_in_chunk as usize..],
            page_record.width,
            page_record.height,
            page_record.flags,
        )
        .expect("generated page decodes");

        for svg in page.commands.iter().filter_map(|command| match command {
            PageCommand::DrawSvgFallback { svg } | PageCommand::DrawSvgTemplate { svg, .. } => {
                Some(svg)
            }
            _ => None,
        }) {
            let mut remaining = svg.as_str();
            while let Some(asset_offset) = remaining.find("hdoc-asset://") {
                let asset_tail = &remaining[asset_offset + "hdoc-asset://".len()..];
                let asset_end = asset_tail
                    .find(['\'', '"', ')', ' ', '\t', '\r', '\n'])
                    .unwrap_or(asset_tail.len());
                let asset_id = asset_tail[..asset_end]
                    .parse::<usize>()
                    .expect("SVG asset reference contains a numeric asset id");
                let media_type = index
                    .assets
                    .get(asset_id)
                    .map(|asset| asset.media_type.as_str());
                assert!(
                    matches!(media_type, Some("image/avif" | "image/png" | "image/jpeg")),
                    "{name} page {} references a valid binary image asset",
                    page_id + 1
                );
                remaining = &asset_tail[asset_end..];
            }

            assert!(
                !svg.contains("data:image/png;base64,")
                    && !svg.contains("data:image/jpeg;base64,")
                    && !svg.contains("data:image/avif;base64,"),
                "{name} page {} contains unextracted Base64 raster image data",
                page_id + 1
            );
        }
    }
}

#[test]
fn chapter_one_pdf_encodes() {
    encode_and_inspect("CH1.pdf");
}

#[test]
fn chapter_two_pdf_encodes() {
    encode_and_inspect("CH2.pdf");
}

#[test]
fn int203_ch02_pdf_encodes() {
    encode_and_inspect("INT203-Ch02.pdf");
}

#[test]
fn int203_ch03_pdf_encodes() {
    encode_and_inspect("INT203-Ch03.pdf");
}
