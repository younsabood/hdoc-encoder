use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    svg_compiler::{collect_avif_asset_uses, collect_direct_path_uses, compile_svg_with_usage},
    EncoderError, HdocEncoderBuilder, PageCommand, PageDisplayList,
};

const RASTER_DATA_URIS: [(&str, &str); 3] = [
    ("data:image/png;base64,", "PNG"),
    ("data:image/jpeg;base64,", "JPEG"),
    ("data:image/jpg;base64,", "JPEG"),
];
const AVIF_DATA_URI: &str = "data:image/avif;base64,";

/// Controls optional lossy compression of raster images embedded in PDF pages.
/// Text, glyph outlines, vector paths, and page geometry remain SVG vectors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfImportOptions {
    /// AVIF visual quality from 1 (smallest) through 100 (highest).
    pub avif_quality: f32,
    /// AVIF encoder speed from 1 (slowest/smallest) through 10 (fastest/larger).
    pub avif_speed: u8,
    /// Convert embedded PNG and JPEG images to AVIF.
    pub convert_embedded_images_to_avif: bool,
}

impl Default for PdfImportOptions {
    fn default() -> Self {
        Self {
            avif_quality: 32.0,
            avif_speed: 6,
            convert_embedded_images_to_avif: true,
        }
    }
}

impl PdfImportOptions {
    fn validate(self) -> Result<Self, EncoderError> {
        if !self.avif_quality.is_finite() || !(1.0..=100.0).contains(&self.avif_quality) {
            return Err(EncoderError::InvalidOption(
                "AVIF quality must be between 1 and 100",
            ));
        }
        if !(1..=10).contains(&self.avif_speed) {
            return Err(EncoderError::InvalidOption(
                "AVIF speed must be between 1 and 10",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfImportStats {
    pub page_count: u32,
    pub extracted_text_bytes: u64,
    /// Total size of the vector SVG page representations.
    pub svg_vector_bytes: u64,
    /// Number of PNG raster images converted to AVIF.
    pub png_images_converted: u32,
    /// Number of JPEG raster images converted to AVIF.
    pub jpeg_images_converted: u32,
    /// Combined encoded byte size of raster images before conversion.
    pub source_image_bytes: u64,
    /// Combined output byte size of converted AVIF images.
    pub avif_image_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RasterConversionStats {
    png_images_converted: u32,
    jpeg_images_converted: u32,
    source_image_bytes: u64,
    avif_image_bytes: u64,
}

impl HdocEncoderBuilder {
    /// Import PDF pages as vector SVG display lists and keep extracted Unicode
    /// text in the separate searchable text block. Embedded PNG and JPEG images are
    /// converted to AVIF while text and vector paths remain vectors.
    pub fn import_pdf_bytes(&mut self, bytes: &[u8]) -> Result<PdfImportStats, EncoderError> {
        self.import_pdf_bytes_with_options(bytes, PdfImportOptions::default())
    }

    pub fn import_pdf_bytes_with_options(
        &mut self,
        bytes: &[u8],
        options: PdfImportOptions,
    ) -> Result<PdfImportStats, EncoderError> {
        let options = options.validate()?;
        use hayro_svg::hayro_interpret::hayro_syntax::Pdf;

        let texts = catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem_by_pages(bytes)
        }))
        .map_err(|_| EncoderError::PdfIngest("text extractor panicked".into()))?
        .map_err(|error| EncoderError::PdfIngest(format!("text extraction failed: {error}")))?;
        let pdf = Pdf::new(bytes.to_vec()).map_err(|error| {
            EncoderError::PdfIngest(format!("PDF parser rejected input: {error:?}"))
        })?;
        let pages = pdf.pages();
        if pages.is_empty() {
            return Err(EncoderError::PdfIngest("PDF contains no pages".into()));
        }
        if pages.len() > u32::MAX as usize
            || self.pages.len().saturating_add(pages.len()) > u32::MAX as usize
        {
            return Err(EncoderError::PdfIngest(
                "PDF page count exceeds the HDOC limit".into(),
            ));
        }

        let cache = hayro_svg::RenderCache::new();
        let interpreter_settings = hayro_svg::hayro_interpret::InterpreterSettings::default();
        let svg_settings = hayro_svg::SvgRenderSettings::default();
        let mut converted = Vec::with_capacity(pages.len());
        let mut svg_vector_bytes = 0u64;
        let mut raster_stats = RasterConversionStats::default();
        let mut extracted_text_bytes = 0u64;

        for (page_index, page) in pages.iter().enumerate() {
            let page_number = page_index + 1;
            let svg = catch_unwind(AssertUnwindSafe(|| {
                hayro_svg::convert(page, &cache, &interpreter_settings, &svg_settings)
            }))
            .map_err(|_| {
                EncoderError::PdfIngest(format!("SVG conversion panicked on page {page_number}"))
            })?;
            let (svg, page_raster_stats) =
                convert_svg_raster_images_to_avif(&svg, options, page_number)?;
            let svg = svag::minify_with_options(
                &svg,
                &svag::Options {
                    precision: 3,
                    ..svag::Options::default()
                },
            )
            .map_err(|error| {
                EncoderError::PdfIngest(format!(
                    "SVG vector optimization failed on page {page_number}: {error}"
                ))
            })?;
            let (width, height) = page.render_dimensions();
            let width = page_dimension(width, page_number)?;
            let height = page_dimension(height, page_number)?;
            let text = texts.get(page_index).cloned().unwrap_or_default();
            svg_vector_bytes = svg_vector_bytes.saturating_add(svg.len() as u64);
            raster_stats.png_images_converted = raster_stats
                .png_images_converted
                .saturating_add(page_raster_stats.png_images_converted);
            raster_stats.jpeg_images_converted = raster_stats
                .jpeg_images_converted
                .saturating_add(page_raster_stats.jpeg_images_converted);
            raster_stats.source_image_bytes = raster_stats
                .source_image_bytes
                .saturating_add(page_raster_stats.source_image_bytes);
            raster_stats.avif_image_bytes = raster_stats
                .avif_image_bytes
                .saturating_add(page_raster_stats.avif_image_bytes);
            extracted_text_bytes = extracted_text_bytes.saturating_add(text.len() as u64);
            converted.push((svg, width, height, text));
        }

        let mut direct_path_uses = HashMap::new();
        let mut avif_asset_uses = HashMap::new();
        for (svg, _, _, _) in &converted {
            collect_direct_path_uses(svg, &mut direct_path_uses);
            collect_avif_asset_uses(svg, &mut avif_asset_uses)?;
        }
        let mut pages = Vec::with_capacity(converted.len());
        for (svg, width, height, text) in converted {
            let mut display_list = PageDisplayList::new(width, height);
            match compile_svg_with_usage(
                &svg,
                &mut self.assets,
                &direct_path_uses,
                &avif_asset_uses,
            )? {
                Some(compiled) => display_list.push(PageCommand::DrawSvgTemplate {
                    svg: compiled.template,
                    glyph_styles: compiled.glyph_styles,
                    glyphs: compiled.glyphs,
                }),
                None => display_list.push(PageCommand::DrawSvgFallback { svg }),
            }
            pages.push((display_list, text));
        }

        let page_count = u32::try_from(pages.len())
            .map_err(|_| EncoderError::PdfIngest("PDF page count exceeds the HDOC limit".into()))?;
        for (display_list, text) in pages {
            self.add_page_with_text(display_list, text)?;
        }

        Ok(PdfImportStats {
            page_count,
            extracted_text_bytes,
            svg_vector_bytes,
            png_images_converted: raster_stats.png_images_converted,
            jpeg_images_converted: raster_stats.jpeg_images_converted,
            source_image_bytes: raster_stats.source_image_bytes,
            avif_image_bytes: raster_stats.avif_image_bytes,
        })
    }

    pub fn import_pdf_path(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<PdfImportStats, EncoderError> {
        self.import_pdf_path_with_options(path, PdfImportOptions::default())
    }

    pub fn import_pdf_path_with_options(
        &mut self,
        path: impl AsRef<std::path::Path>,
        options: PdfImportOptions,
    ) -> Result<PdfImportStats, EncoderError> {
        let bytes = std::fs::read(path)?;
        self.import_pdf_bytes_with_options(&bytes, options)
    }
}

fn convert_svg_raster_images_to_avif(
    svg: &str,
    options: PdfImportOptions,
    page_number: usize,
) -> Result<(String, RasterConversionStats), EncoderError> {
    if !options.convert_embedded_images_to_avif
        || !RASTER_DATA_URIS
            .iter()
            .any(|(data_uri, _)| svg.contains(data_uri))
    {
        return Ok((svg.to_owned(), RasterConversionStats::default()));
    }

    let mut output = String::with_capacity(svg.len());
    let mut remaining = svg;
    let mut stats = RasterConversionStats::default();

    while let Some((prefix_offset, data_uri, source_format)) = RASTER_DATA_URIS
        .iter()
        .filter_map(|(data_uri, source_format)| {
            remaining
                .find(data_uri)
                .map(|offset| (offset, *data_uri, *source_format))
        })
        .min_by_key(|(offset, _, _)| *offset)
    {
        let data_start = prefix_offset + data_uri.len();
        let payload = &remaining[data_start..];
        let payload_end = payload
            .find(['\'', '"'])
            .ok_or_else(|| EncoderError::PdfIngest("unterminated SVG image data URI".into()))?;
        let encoded_image = &payload[..payload_end];
        let image_bytes = STANDARD.decode(encoded_image).map_err(|error| {
            EncoderError::PdfIngest(format!(
                "invalid embedded {source_format} on page {page_number}: {error}"
            ))
        })?;
        let avif = encode_embedded_raster(&image_bytes, source_format, options, page_number)?;

        output.push_str(&remaining[..prefix_offset]);
        output.push_str(AVIF_DATA_URI);
        output.push_str(&STANDARD.encode(&avif));
        stats.source_image_bytes = stats
            .source_image_bytes
            .saturating_add(image_bytes.len() as u64);
        stats.avif_image_bytes = stats.avif_image_bytes.saturating_add(avif.len() as u64);
        if source_format == "PNG" {
            stats.png_images_converted = stats.png_images_converted.saturating_add(1);
        } else {
            stats.jpeg_images_converted = stats.jpeg_images_converted.saturating_add(1);
        }
        remaining = &payload[payload_end..];
    }
    output.push_str(remaining);
    Ok((output, stats))
}

fn encode_embedded_raster(
    image_bytes: &[u8],
    source_format: &str,
    options: PdfImportOptions,
    page_number: usize,
) -> Result<Vec<u8>, EncoderError> {
    let decoded = catch_unwind(AssertUnwindSafe(|| image::load_from_memory(image_bytes)))
        .map_err(|_| {
            EncoderError::PdfIngest(format!(
                "{source_format} decoding panicked on page {page_number}"
            ))
        })?
        .map_err(|error| {
            EncoderError::PdfIngest(format!(
                "{source_format} decoding failed on page {page_number}: {error}"
            ))
        })?
        .to_rgba8();
    let width = decoded.width() as usize;
    let height = decoded.height() as usize;
    let pixels = decoded
        .pixels()
        .map(|pixel| ravif::RGBA8::new(pixel[0], pixel[1], pixel[2], pixel[3]))
        .collect::<Vec<_>>();
    catch_unwind(AssertUnwindSafe(|| {
        ravif::Encoder::new()
            .with_quality(options.avif_quality)
            .with_speed(options.avif_speed)
            .encode_rgba(ravif::Img::new(&pixels, width, height))
    }))
    .map_err(|_| EncoderError::PdfIngest(format!("AVIF encoding panicked on page {page_number}")))?
    .map(|image| image.avif_file)
    .map_err(|error| {
        EncoderError::PdfIngest(format!(
            "AVIF encoding failed on page {page_number}: {error}"
        ))
    })
}

fn page_dimension(value: f32, page_number: usize) -> Result<u16, EncoderError> {
    if !value.is_finite() || value <= 0.0 || value.round() > u16::MAX as f32 {
        return Err(EncoderError::PdfIngest(format!(
            "page {page_number} has dimensions outside the HDOC page range"
        )));
    }
    Ok(value.round() as u16)
}
