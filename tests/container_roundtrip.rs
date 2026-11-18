use std::io::Cursor;

use hdoc_encoder::{
    format::{CentralIndex, FileHeader, FLAG_ARCHIVAL, FLAG_HAS_SEARCH_INDEX, HEADER_SIZE},
    opcodes::decode_page,
    AssetInput, CompressionProfile, GlyphOffset, HdocEncoderBuilder, PageCommand, PageDisplayList,
    SearchBlock, SvgGlyphStyle, SvgGlyphUse,
};

#[test]
fn streaming_seekable_encode_matches_in_memory_encode_and_deduplicates_assets() {
    assert_eq!(CompressionProfile::default(), CompressionProfile::Archival);

    let mut encoder = HdocEncoderBuilder::new();
    encoder.profile(CompressionProfile::UltraLight);
    encoder.chunk_limits(2, 64 * 1024).unwrap();
    encoder.metadata("title", "HyperDoc Test Suite");

    let first_id = encoder
        .add_asset(AssetInput::new(b"shared-png-bytes".to_vec(), "image/png"))
        .unwrap();
    let dup_id = encoder
        .add_asset(
            AssetInput::new(b"shared-png-bytes".to_vec(), "image/png").with_dimensions(640, 480),
        )
        .unwrap();
    assert_eq!(first_id, dup_id);

    for page_num in 0..5u16 {
        let mut page = PageDisplayList::new(612, 792);
        page.push(PageCommand::SetColor { rgba: 0x1122_33ff });
        page.draw_glyph_run(
            vec![10 + page_num, 20 + page_num],
            vec![GlyphOffset { dx: 0, dy: 0 }, GlyphOffset { dx: 32, dy: 0 }],
        )
        .unwrap();
        page.push(PageCommand::DrawImage {
            asset_id: first_id,
            x: 10,
            y: 20,
            width: 640,
            height: 480,
        });
        encoder
            .add_page_with_text(
                page,
                format!("صفحة رقم {page_num} ﻻ ﺇﻟﻪ ﺇﻻ \u{FDF2} مدرسة \u{FB01}le"),
            )
            .unwrap();
    }

    let in_memory = encoder.encode().unwrap();
    let mut streamed = Cursor::new(Vec::new());
    let stream_stats = encoder.write_to_seekable(&mut streamed).unwrap();
    assert_eq!(in_memory.stats, stream_stats);
    assert_eq!(in_memory.bytes, streamed.into_inner());
    assert_eq!(in_memory.stats.chunk_count, 3);
    assert_eq!(in_memory.stats.unique_asset_count, 1);
    assert_eq!(in_memory.stats.duplicate_asset_count, 1);

    let header = FileHeader::decode(&in_memory.bytes[..HEADER_SIZE]).unwrap();
    assert_eq!(header.flags, FLAG_HAS_SEARCH_INDEX);
    assert_eq!(header.flags & FLAG_ARCHIVAL, 0);

    let footer_start = header.footer_offset as usize;
    let footer_end = footer_start + header.footer_size as usize;
    let raw_index =
        zstd::bulk::decompress(&in_memory.bytes[footer_start..footer_end], 8 * 1024 * 1024)
            .unwrap();
    let index = CentralIndex::decode(&raw_index).unwrap();
    assert_eq!(index.assets[0].width, 640);
    assert_eq!(index.assets[0].height, 480);

    let search_slice = &in_memory.bytes[index.search_block.offset as usize
        ..index.search_block.offset as usize + index.search_block.compressed_size as usize];
    let raw_search =
        zstd::bulk::decompress(search_slice, index.search_block.uncompressed_size as usize)
            .unwrap();
    let search = SearchBlock::decode(&raw_search).unwrap();
    assert_eq!(search.postings.get("الله").map(Vec::len), Some(5));
    assert_eq!(search.postings.get("مدرسه").map(Vec::len), Some(5));
    assert_eq!(search.postings.get("file").map(Vec::len), Some(5));

    let chunk0 = &index.chunks[0];
    let raw_chunk0 = zstd::bulk::decompress(
        &in_memory.bytes
            [chunk0.offset as usize..chunk0.offset as usize + chunk0.compressed_size as usize],
        chunk0.uncompressed_size as usize,
    )
    .unwrap();
    let (decoded_page0, _) = decode_page(
        &raw_chunk0[index.pages[0].offset_in_chunk as usize..],
        index.pages[0].width,
        index.pages[0].height,
        index.pages[0].flags,
    )
    .unwrap();
    assert_eq!(decoded_page0.commands.len(), 3);
}

#[test]
fn rejects_unsafe_svg_presentation_during_encode() {
    let mut page = PageDisplayList::new(400, 400);
    page.push(PageCommand::DrawSvgTemplate {
        svg: "<svg><!--HDOC_GLYPH--></svg>".into(),
        glyph_styles: vec![SvgGlyphStyle {
            matrix: [1.0, 0.0, 0.0, 1.0],
            presentation: "onclick=\"evil()\"".into(),
        }],
        glyphs: vec![SvgGlyphUse {
            asset_id: 0,
            style_id: 0,
            dx: 0,
            dy: 0,
        }],
    });
    let mut encoder = HdocEncoderBuilder::new();
    encoder.add_page(page).unwrap();
    assert!(encoder.encode().is_err());
}
