# Quick start

## Create a page

Add the crate from GitHub:

```toml
[dependencies]
hdoc-encoder = { git = "https://github.com/younsabood/hdoc-encoder" }
```

Build a display list, attach its Unicode text separately, and write the result:

```rust,no_run
use hdoc_encoder::{GlyphOffset, HdocEncoderBuilder, PageDisplayList};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut page = PageDisplayList::new(612, 792);
    page.draw_glyph_run(
        vec![65, 66, 67],
        vec![
            GlyphOffset { dx: 0, dy: 0 },
            GlyphOffset { dx: 24, dy: 0 },
            GlyphOffset { dx: 25, dy: 0 },
        ],
    )?;

    let mut encoder = HdocEncoderBuilder::new();
    encoder.add_page_with_text(page, "ABC")?;
    encoder.metadata("title", "Example");
    let stats = encoder.write_to_path("example.hdoc")?;
    println!("wrote {} bytes", stats.output_bytes);
    Ok(())
}
```

Glyph IDs are drawing data; the Unicode string is stored in the search index.
Callers are responsible for shaping text and supplying valid glyph IDs and
offsets.

## Import a PDF

Enable `pdf-ingest` and import a path:

```toml
[dependencies]
hdoc-encoder = { git = "https://github.com/younsabood/hdoc-encoder", features = ["pdf-ingest"] }
```

```rust,no_run
use hdoc_encoder::HdocEncoderBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = HdocEncoderBuilder::new();
    let imported = encoder.import_pdf_path("input.pdf")?;
    let output = encoder.write_to_path("output.hdoc")?;
    println!("{} pages, {} output bytes", imported.page_count, output.output_bytes);
    Ok(())
}
```

See [PDF import](pdf-ingest.md) for the image conversion settings and sample
test fixture configuration.
