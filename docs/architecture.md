# Architecture

The encoder is a standalone crate. Its file-format and opcode modules live in
this repository and are not imported from the decoder project. The two
implementations are kept separate by design; changes to the binary format must
be reflected and compatibility-tested in both repositories.

## Source modules

| Module | Responsibility |
| --- | --- |
| `builder.rs` | Validate and collect pages, page text, metadata, and options. |
| `asset_pool.rs` | Hash and deduplicate identical asset payloads. |
| `opcodes.rs` | Serialize display-list commands into page bytecode. |
| `svg_compiler.rs` | Compile reusable SVG glyph paths and repeated image references; enabled with PDF import. |
| `vector_path.rs` | Encode SVG path geometry as HPTH bytecode; enabled with PDF import. |
| `pdf_ingest.rs` | Convert PDF pages to SVG display lists and extract text; feature-gated. |
| `writer.rs` | Prepare vector assets, write chunks, search data, index, header, and trailer. |
| `format.rs` | Encode the encoder-side HDOC header and central-index schema. |
| `search.rs` | Build the searchable text block and token postings. |

## Encode stages

1. The caller supplies display lists and assets, or enables the PDF importer.
2. BLAKE3 fingerprints deduplicate identical assets.
3. Reused SVG paths are encoded as HPTH assets. If it saves space, the writer
   trains one Zstandard dictionary and compresses the path assets with it.
4. Page opcodes are grouped into solid chunks and compressed independently.
5. The optional search block and central index are compressed and written before
   the fixed end marker. The header is finalized with counts and offsets.

PDF import keeps vector graphics as SVG/path data and stores extracted Unicode
text separately for search. Embedded PNG/JPEG images can be converted to AVIF.
See [PDF import](pdf-ingest.md) for current scope and limitations.

## File layout

The physical order is header, asset payloads, compressed page chunks, optional
compressed search block, compressed central index, and end marker. The index
provides random access to each page and asset. See the [format guide](format.md)
and [file layout diagram](diagrams/file-layout.mmd).
