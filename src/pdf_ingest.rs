use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::atomic::{AtomicUsize, Ordering},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    svg_compiler::{collect_direct_path_uses, compile_svg_with_usage},
    AssetInput, EncoderError, HdocEncoderBuilder, PageCommand, PageDisplayList,
};

const RASTER_DATA_URIS: [(&str, &str, &str); 3] = [
    ("data:image/png;base64,", "PNG", "image/png"),
    ("data:image/jpeg;base64,", "JPEG", "image/jpeg"),
    ("data:image/jpg;base64,", "JPEG", "image/jpeg"),
];
const ASSET_URI_PREFIX: &str = "hdoc-asset://";

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
    /// Keep original PNG or JPEG bytes when AVIF output is larger than source.
    pub prefer_original_if_smaller: bool,
}

impl Default for PdfImportOptions {
    fn default() -> Self {
        Self {
            avif_quality: 32.0,
            avif_speed: 6,
            convert_embedded_images_to_avif: true,
            prefer_original_if_smaller: false,
        }
    }
}

impl PdfImportOptions {
    pub fn with_smart_size_guard(mut self, enabled: bool) -> Self {
        self.prefer_original_if_smaller = enabled;
        self
    }

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

#[derive(Debug)]
struct RawRasterRef {
    hash: [u8; 32],
    bytes: Vec<u8>,
    source_format: &'static str,
    source_mime: &'static str,
    first_page_number: usize,
}

#[derive(Debug)]
struct TranscodedRaster {
    bytes: Vec<u8>,
    media_type: &'static str,
    width: u32,
    height: u32,
    converted_to_avif: bool,
}

#[derive(Debug)]
struct OptimizedPage {
    svg: String,
    width: u16,
    height: u16,
    text: String,
    direct_path_uses: HashMap<[u8; 32], usize>,
}

impl HdocEncoderBuilder {
    /// Import PDF pages as vector SVG display lists and keep extracted Unicode
    /// text in the separate searchable text block. Embedded raster images are
    /// stored directly as deduplicated binary assets without Base64 bloat.
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

        let mut raw_pages = Vec::with_capacity(pages.len());
        let mut unique_rasters = Vec::<RawRasterRef>::new();
        let mut raster_index_by_hash = HashMap::<[u8; 32], usize>::new();
        let mut png_occurrences = 0u32;
        let mut jpeg_occurrences = 0u32;
        let mut source_image_bytes = 0u64;
        let mut extracted_text_bytes = 0u64;

        for (page_index, page) in pages.iter().enumerate() {
            let page_number = page_index + 1;
            let raw_svg = catch_unwind(AssertUnwindSafe(|| {
                hayro_svg::convert(page, &cache, &interpreter_settings, &svg_settings)
            }))
            .map_err(|_| {
                EncoderError::PdfIngest(format!("SVG conversion panicked on page {page_number}"))
            })?;
            let (width, height) = page.render_dimensions();
            let width = page_dimension(width, page_number)?;
            let height = page_dimension(height, page_number)?;
            let text = texts.get(page_index).cloned().unwrap_or_default();
            extracted_text_bytes = extracted_text_bytes.saturating_add(text.len() as u64);

            let (stripped_svg, page_refs) = extract_page_raster_placeholders(
                &raw_svg,
                page_number,
                &mut unique_rasters,
                &mut raster_index_by_hash,
            )?;
            for (raster_idx, format_label, byte_len) in &page_refs {
                source_image_bytes = source_image_bytes.saturating_add(*byte_len as u64);
                if *format_label == "PNG" {
                    png_occurrences = png_occurrences.saturating_add(1);
                } else {
                    jpeg_occurrences = jpeg_occurrences.saturating_add(1);
                }
                let _ = raster_idx;
            }
            raw_pages.push((stripped_svg, page_refs, width, height, text, page_number));
        }

        let transcoded = transcode_unique_rasters_parallel(&unique_rasters, options)?;
        let mut asset_ids = Vec::with_capacity(transcoded.len());
        let mut avif_by_raster = Vec::with_capacity(transcoded.len());
        for item in transcoded {
            let avif_len = if item.converted_to_avif {
                item.bytes.len() as u64
            } else {
                0
            };
            let asset_id = self.assets.insert(
                AssetInput::new(item.bytes, item.media_type)
                    .with_dimensions(item.width, item.height),
            )?;
            asset_ids.push(asset_id);
            avif_by_raster.push((item.converted_to_avif, avif_len));
        }

        let mut png_images_converted = 0u32;
        let mut jpeg_images_converted = 0u32;
        let mut avif_image_bytes = 0u64;
        let mut prepared_inputs = Vec::with_capacity(raw_pages.len());
        for (stripped_svg, page_refs, width, height, text, page_number) in raw_pages {
            let mut resolved_svg = stripped_svg;
            for (slot, (raster_idx, format_label, _)) in page_refs.iter().enumerate() {
                let placeholder = format!("__hdoc_raster_slot_{slot}__");
                let asset_uri = format!("{ASSET_URI_PREFIX}{}", asset_ids[*raster_idx]);
                resolved_svg = resolved_svg.replacen(&placeholder, &asset_uri, 1);
                let (was_avif, avif_len) = avif_by_raster[*raster_idx];
                if was_avif {
                    avif_image_bytes = avif_image_bytes.saturating_add(avif_len);
                    if *format_label == "PNG" {
                        png_images_converted = png_images_converted.saturating_add(1);
                    } else {
                        jpeg_images_converted = jpeg_images_converted.saturating_add(1);
                    }
                }
            }
            prepared_inputs.push((resolved_svg, width, height, text, page_number));
        }
        let _ = (png_occurrences, jpeg_occurrences);

        let optimized_pages = minify_pages_parallel(prepared_inputs)?;
        let mut direct_path_uses = HashMap::<[u8; 32], usize>::new();
        let mut svg_vector_bytes = 0u64;
        for page in &optimized_pages {
            svg_vector_bytes = svg_vector_bytes.saturating_add(page.svg.len() as u64);
            for (hash, count) in &page.direct_path_uses {
                *direct_path_uses.entry(*hash).or_insert(0) += *count;
            }
        }

        let page_count = u32::try_from(optimized_pages.len())
            .map_err(|_| EncoderError::PdfIngest("PDF page count exceeds the HDOC limit".into()))?;
        for page in optimized_pages {
            let mut display_list = PageDisplayList::new(page.width, page.height);
            match compile_svg_with_usage(&page.svg, &mut self.assets, &direct_path_uses)? {
                Some(compiled) => display_list.push(PageCommand::DrawSvgTemplate {
                    svg: compiled.template,
                    glyph_styles: compiled.glyph_styles,
                    glyphs: compiled.glyphs,
                }),
                None => display_list.push(PageCommand::DrawSvgFallback { svg: page.svg }),
            }
            self.add_page_with_text(display_list, page.text)?;
        }

        Ok(PdfImportStats {
            page_count,
            extracted_text_bytes,
            svg_vector_bytes,
            png_images_converted,
            jpeg_images_converted,
            source_image_bytes,
            avif_image_bytes,
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

#[allow(clippy::type_complexity)]
fn extract_page_raster_placeholders(
    svg: &str,
    page_number: usize,
    unique_rasters: &mut Vec<RawRasterRef>,
    raster_index_by_hash: &mut HashMap<[u8; 32], usize>,
) -> Result<(String, Vec<(usize, &'static str, usize)>), EncoderError> {
    if !RASTER_DATA_URIS
        .iter()
        .any(|(data_uri, _, _)| svg.contains(data_uri))
    {
        return Ok((svg.to_owned(), Vec::new()));
    }

    let mut output = String::with_capacity(svg.len() / 2);
    let mut remaining = svg;
    let mut page_refs = Vec::new();

    while let Some((prefix_offset, data_uri, source_format, source_mime)) = RASTER_DATA_URIS
        .iter()
        .filter_map(|(data_uri, source_format, source_mime)| {
            remaining
                .find(data_uri)
                .map(|offset| (offset, *data_uri, *source_format, *source_mime))
        })
        .min_by_key(|(offset, _, _, _)| *offset)
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
        let byte_len = image_bytes.len();
        let hash = *blake3::hash(&image_bytes).as_bytes();
        let raster_idx = match raster_index_by_hash.get(&hash).copied() {
            Some(idx) => idx,
            None => {
                let idx = unique_rasters.len();
                unique_rasters.push(RawRasterRef {
                    hash,
                    bytes: image_bytes,
                    source_format,
                    source_mime,
                    first_page_number: page_number,
                });
                raster_index_by_hash.insert(hash, idx);
                idx
            }
        };
        let _ = unique_rasters[raster_idx].hash;
        let slot = page_refs.len();
        page_refs.push((raster_idx, source_format, byte_len));
        output.push_str(&remaining[..prefix_offset]);
        output.push_str(&format!("__hdoc_raster_slot_{slot}__"));
        remaining = &payload[payload_end..];
    }
    output.push_str(remaining);
    Ok((output, page_refs))
}

fn transcode_unique_rasters_parallel(
    rasters: &[RawRasterRef],
    options: PdfImportOptions,
) -> Result<Vec<TranscodedRaster>, EncoderError> {
    if rasters.is_empty() {
        return Ok(Vec::new());
    }
    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(rasters.len())
        .max(1);
    if worker_count == 1 {
        return rasters
            .iter()
            .map(|item| transcode_one_raster(item, options))
            .collect();
    }

    let next_index = AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, Result<TranscodedRaster, EncoderError>)>> =
        std::sync::Mutex::new(Vec::with_capacity(rasters.len()));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let index = next_index.fetch_add(1, Ordering::Relaxed);
                if index >= rasters.len() {
                    break;
                }
                let outcome = transcode_one_raster(&rasters[index], options);
                if let Ok(mut guard) = results.lock() {
                    guard.push((index, outcome));
                }
            });
        }
    });

    let mut collected = results
        .into_inner()
        .map_err(|_| EncoderError::PdfIngest("raster worker lock poisoned".into()))?;
    collected.sort_unstable_by_key(|(index, _)| *index);
    collected.into_iter().map(|(_, res)| res).collect()
}

fn transcode_one_raster(
    raster: &RawRasterRef,
    options: PdfImportOptions,
) -> Result<TranscodedRaster, EncoderError> {
    let page_number = raster.first_page_number;
    let source_format = raster.source_format;
    let decoded = catch_unwind(AssertUnwindSafe(|| image::load_from_memory(&raster.bytes)))
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
    let width = decoded.width();
    let height = decoded.height();

    if !options.convert_embedded_images_to_avif {
        return Ok(TranscodedRaster {
            bytes: raster.bytes.clone(),
            media_type: raster.source_mime,
            width,
            height,
            converted_to_avif: false,
        });
    }

    let pixels = decoded
        .pixels()
        .map(|pixel| ravif::RGBA8::new(pixel[0], pixel[1], pixel[2], pixel[3]))
        .collect::<Vec<_>>();
    let avif = catch_unwind(AssertUnwindSafe(|| {
        ravif::Encoder::new()
            .with_quality(options.avif_quality)
            .with_speed(options.avif_speed)
            .encode_rgba(ravif::Img::new(&pixels, width as usize, height as usize))
    }))
    .map_err(|_| EncoderError::PdfIngest(format!("AVIF encoding panicked on page {page_number}")))?
    .map(|image| image.avif_file)
    .map_err(|error| {
        EncoderError::PdfIngest(format!(
            "AVIF encoding failed on page {page_number}: {error}"
        ))
    })?;

    if options.prefer_original_if_smaller && avif.len() >= raster.bytes.len() {
        return Ok(TranscodedRaster {
            bytes: raster.bytes.clone(),
            media_type: raster.source_mime,
            width,
            height,
            converted_to_avif: false,
        });
    }

    Ok(TranscodedRaster {
        bytes: avif,
        media_type: "image/avif",
        width,
        height,
        converted_to_avif: true,
    })
}

fn minify_pages_parallel(
    pages: Vec<(String, u16, u16, String, usize)>,
) -> Result<Vec<OptimizedPage>, EncoderError> {
    let worker_count = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(pages.len())
        .max(1);
    if worker_count == 1 {
        return pages
            .into_iter()
            .map(|(svg, width, height, text, page_number)| {
                optimize_single_page(&svg, width, height, text, page_number)
            })
            .collect();
    }

    let next_index = AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(usize, Result<OptimizedPage, EncoderError>)>> =
        std::sync::Mutex::new(Vec::with_capacity(pages.len()));

    std::thread::scope(|scope| {
        let slice = pages.as_slice();
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let index = next_index.fetch_add(1, Ordering::Relaxed);
                if index >= slice.len() {
                    break;
                }
                let (ref svg, width, height, ref text, page_number) = slice[index];
                let outcome = optimize_single_page(svg, width, height, text.clone(), page_number);
                if let Ok(mut guard) = results.lock() {
                    guard.push((index, outcome));
                }
            });
        }
    });

    let mut collected = results
        .into_inner()
        .map_err(|_| EncoderError::PdfIngest("SVG minify worker lock poisoned".into()))?;
    collected.sort_unstable_by_key(|(index, _)| *index);
    collected.into_iter().map(|(_, res)| res).collect()
}

fn optimize_single_page(
    svg: &str,
    width: u16,
    height: u16,
    text: String,
    page_number: usize,
) -> Result<OptimizedPage, EncoderError> {
    let minified = svag::minify_with_options(
        svg,
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
    let mut direct_path_uses = HashMap::new();
    collect_direct_path_uses(&minified, &mut direct_path_uses);
    Ok(OptimizedPage {
        svg: minified,
        width,
        height,
        text,
        direct_path_uses,
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
