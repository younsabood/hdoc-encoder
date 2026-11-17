# HyperDoc 1.0 format

All integer fields use little-endian byte order. The encoder writes format
version `0x0100` and a fixed 32-byte header.

## Header

| Byte range | Field |
| --- | --- |
| `0..4` | Magic `HDOC` |
| `4..6` | Version (`u16`) |
| `6..8` | Flags (`u16`): search index and archival profile |
| `8..12` | Page count (`u32`) |
| `12..16` | Asset count (`u32`) |
| `16..24` | Compressed central-index offset (`u64`) |
| `24..28` | Compressed central-index size (`u32`) |
| `28..32` | CRC32 over bytes `0..28` |

## Physical sections

1. The fixed header.
2. Raw asset payloads, described by asset records in the central index.
3. Independent Zstandard page chunks.
4. An optional compressed search block.
5. The compressed central index.
6. The 16-byte `HDOC-END-MARKER!` trailer.

The central index has `HDIX` magic and stores metadata, asset records, chunk
records, the page directory, and the optional search-block record. The page
directory maps a page number to its chunk, byte offset, dimensions, and flags.

## Page commands

Each page ends with opcode `0x08`. Current command tags are:

| Tag | Command |
| --- | --- |
| `0x01` | Set font asset and fixed-point size |
| `0x02` | Set RGBA color |
| `0x03` | Absolute move |
| `0x04` | Delta move using signed ZigZag/LEB128 values |
| `0x05` | Shaped glyph IDs with signed offsets |
| `0x06` | Draw image asset with position and dimensions |
| `0x07` | Draw vector asset with a six-value transform |
| `0x08` | End page |
| `0x09` | SVG fallback |
| `0x0a` | SVG template with style table and reusable glyph-path references |

HPTH path assets have their own versioned bytecode and media type. Large or
unsupported PDF SVG constructs can remain in the SVG fallback, preserving the
original vector representation instead of rasterizing text.

This document describes the encoder's current schema. The [decoder repository](https://github.com/younsabood/hdoc-decoder)
contains its own independent implementation; keep both readers and writers
compatible when changing this format.
