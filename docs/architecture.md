# Architecture

The encoder is a standalone crate. Its file-format and opcode modules live in
this repository and are not imported from the decoder project. The two
implementations are kept separate by design; changes to the binary format must
be reflected and compatibility-tested in both repositories.

## Source modules

| Module | Responsibility |
| --- | --- |
| `builder.rs` | Validate and collect pages, page text, metadata, and options. |
| `asset_pool.rs` | Hash and deduplicate identical asset payloads while preserving dimensions. |
| `opcodes.rs` | Validate and serialize display-list commands into page bytecode. |
| `svg_compiler.rs` | Stack-parse SVG structure, full transform lists, binary image assets, and reusable glyph paths. |
| `vector_path.rs` | Encode scale-normalized relative-delta SVG path geometry as HPTH bytecode. |
| `pdf_ingest.rs` | Deduplicate and transcode PDF images in parallel, minify vector SVG pages, and extract text. |
| `writer.rs` | Stream vector assets, parallel-compressed chunks, compact search postings, index, and header. |
| `format.rs` | Encode and validate the HDOC header and central-index schema. |
| `search.rs` | Normalize Unicode/Arabic presentation forms and encode delta-varint TF postings. |

## Encode stages

1. The caller supplies display lists and assets, or enables the PDF importer.
2. BLAKE3 fingerprints deduplicate identical assets (including single-use and
   repeated raster images, avoiding Base64 overhead).
3. Reused and proportionally scaled SVG paths are encoded as relative-delta
   HPTH assets. If it saves space, the writer trains one Zstandard dictionary
   and compresses the path assets with it.
4. Page opcodes are serialized and grouped into solid chunks compressed in
   parallel across CPU threads.
5. Output streams directly to `Write + Seek` targets, finalizing the 32-byte
   CRC32 header at offset zero after writing the central index and end marker.

## File layout

The physical order is header, asset payloads, compressed page chunks, optional
compressed search block, compressed central index, and end marker. The index
provides random access to each page and asset. See the [format guide](format.md)
and [file layout diagram](diagrams/file-layout.mmd).
