use std::{env, error::Error, path::PathBuf};

use hdoc_encoder::HdocEncoderBuilder;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let usage = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: pdf_to_hdoc <input.pdf> <output.hdoc>",
        )
    };
    let input = args.next().ok_or_else(usage)?;
    let output = args.next().ok_or_else(usage)?;
    let mut encoder = HdocEncoderBuilder::new();
    let imported = encoder.import_pdf_path(PathBuf::from(input))?;
    let stats = encoder.write_to_path(output)?;
    println!(
        "pages={} svg_template_input_bytes={} png_to_avif={} jpeg_to_avif={} source_image_bytes={} avif_image_bytes={} text_bytes={} assets={} duplicate_assets={} page_payload_bytes={} hdoc_bytes={}",
        imported.page_count,
        imported.svg_vector_bytes,
        imported.png_images_converted,
        imported.jpeg_images_converted,
        imported.source_image_bytes,
        imported.avif_image_bytes,
        imported.extracted_text_bytes,
        stats.unique_asset_count,
        stats.duplicate_asset_count,
        stats.input_payload_bytes,
        stats.output_bytes
    );
    Ok(())
}
