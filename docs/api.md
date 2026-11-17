# API reference

## `HdocEncoderBuilder`

The builder owns the pages, searchable text, metadata, assets, chunk settings,
and compression profile for one output file.

| Method | Purpose |
| --- | --- |
| `new()` | Create a builder with archival compression and search indexing enabled. |
| `add_page(page)` | Add a page and return its zero-based page ID. |
| `add_page_with_text(page, text)` | Add a page and its logical Unicode text. |
| `page_text(page_id, text)` | Set text for an existing page. |
| `add_asset(asset)` | Add an asset; identical byte payloads share an asset ID. |
| `metadata(key, value)` | Attach string metadata to the container. |
| `profile(profile)` | Select archival, balanced, or ultra-light compression. |
| `chunk_limits(max_pages, target_bytes)` | Set per-chunk page and raw-byte limits. |
| `include_search_index(enabled)` | Enable or omit the document text index. |
| `encode()` | Return the complete output bytes and encoding statistics. |
| `write_to(writer)` | Write to any `std::io::Write`. |
| `write_to_path(path)` | Write the HDOC file to disk. |

## Page data

`PageDisplayList` stores dimensions, flags, and ordered `PageCommand` values.
Commands include font/color state, absolute or delta moves, shaped glyph runs,
images, vectors, SVG fallbacks, and SVG templates with reused path references.
Use `draw_glyph_run` to validate that glyph IDs and offsets have equal lengths.

## Compression settings

`CompressionProfile::Archival` uses Zstandard level 22. `Balanced` uses level 19,
and `UltraLight` uses level 9. The default chunk target is 64 pages or 8 MiB of
uncompressed page opcodes, whichever is reached first.

`EncodingStats` reports pages, unique and duplicate assets, input payload bytes,
output bytes, and chunk count. See the [format guide](format.md) for details on
the binary structures.
