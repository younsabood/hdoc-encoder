# PDF import

PDF ingestion is behind the optional `pdf-ingest` feature, so users who build
from existing display lists do not compile the PDF and image dependencies.

```toml
hdoc-encoder = { git = "https://github.com/younsabood/hdoc-encoder", features = ["pdf-ingest"] }
```

The importer converts pages to vector SVG display lists, extracts searchable
Unicode text, converts supported embedded PNG/JPEG images to AVIF when enabled,
and compiles reusable SVG path outlines to HPTH assets. It does not embed
subset fonts; text outlines remain vector paths.

`PdfImportOptions` controls AVIF quality and speed and can disable conversion of
embedded images. Defaults use quality 32 and speed 6. Images and paths remain
separate from the logical page text used by the search index.

## Local PDF tests

The integration suite recognizes these development samples:

- `CH1.pdf`
- `CH2.pdf`
- `INT203-Ch02.pdf`
- `INT203-Ch03.pdf`

The PDF files are intentionally not part of this repository. To run the
integration cases in a standalone checkout, set `HDOC_PDF_TEST_DIR` to the
directory containing the sample files. When they are absent, only those sample
cases are skipped; the rest of the crate remains testable without private or
large fixture PDFs.
