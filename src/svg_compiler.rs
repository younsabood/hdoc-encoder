use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    asset_pool::AssetPool, vector_path::EncodedPath, AssetInput, EncoderError, SvgGlyphStyle,
    SvgGlyphUse,
};

const GLYPH_MARKER: &str = "<!--HDOC_GLYPH-->";
const AVIF_DATA_URI_PREFIX: &str = "data:image/avif;base64,";
const ASSET_URI_PREFIX: &str = "hdoc-asset://";
const DIRECT_PATH_ASSET_MIN_USES: usize = 3;
const DIRECT_PATH_ASSET_MIN_BYTES: usize = 16;
const VECTOR_PATH_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path;version=1";
const VECTOR_PATH_ZSTD_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path+zstd;version=1";
const PRESENTATION_ATTRIBUTES: &[&str] = &[
    "fill",
    "fill-rule",
    "fill-opacity",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-opacity",
    "opacity",
    "vector-effect",
    "paint-order",
    "clip-rule",
    "clip-path",
    "mask",
    "style",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledSvg {
    pub template: String,
    pub glyph_styles: Vec<SvgGlyphStyle>,
    pub glyphs: Vec<SvgGlyphUse>,
}

#[derive(Debug, Clone)]
struct Tag {
    start: usize,
    end: usize,
    name: String,
    attributes: Vec<(String, String)>,
    attribute_value_ranges: Vec<(String, usize, usize)>,
}

type ParsedTag = (
    usize,
    String,
    Vec<(String, String)>,
    Vec<(String, usize, usize)>,
);

#[derive(Debug, Clone)]
struct PathDefinition {
    tag_index: usize,
    path: EncodedPath,
    asset_id: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

#[derive(Debug, Clone)]
struct Replacement {
    start: usize,
    end: usize,
    value: String,
}

#[cfg(test)]
fn compile_svg(svg: &str, assets: &mut AssetPool) -> Result<Option<CompiledSvg>, EncoderError> {
    let mut direct_path_uses = HashMap::new();
    collect_direct_path_uses(svg, &mut direct_path_uses);
    let mut avif_asset_uses = HashMap::new();
    collect_avif_asset_uses(svg, &mut avif_asset_uses)?;
    compile_svg_with_usage(svg, assets, &direct_path_uses, &avif_asset_uses)
}

pub(crate) fn compile_svg_with_usage(
    svg: &str,
    assets: &mut AssetPool,
    direct_path_uses: &HashMap<[u8; 32], usize>,
    avif_asset_uses: &HashMap<[u8; 32], usize>,
) -> Result<Option<CompiledSvg>, EncoderError> {
    let tags = find_svg_tags(svg);
    let mut path_definitions = HashMap::<String, PathDefinition>::new();

    for (tag_index, tag) in tags.iter().enumerate() {
        if tag.name != "path"
            || !svg[tag.start..tag.end].trim_end().ends_with("/>")
            || !is_inside(svg, tag.start, "defs")
        {
            continue;
        }
        let attrs = attribute_map(tag);
        let (Some(id), Some(data)) = (attrs.get("id"), attrs.get("d")) else {
            continue;
        };
        if attrs.len() != 2 {
            continue;
        }
        let Some(path) = crate::vector_path::encode_path_data_unscaled(data) else {
            continue;
        };
        path_definitions.insert(
            id.clone(),
            PathDefinition {
                tag_index,
                path,
                asset_id: None,
            },
        );
    }

    let mut reference_counts = HashMap::<String, usize>::new();
    for tag in &tags {
        if tag.name != "use" || !svg[tag.start..tag.end].trim_end().ends_with("/>") {
            continue;
        }
        if let Some(id) = use_reference(tag) {
            if path_definitions.contains_key(&id) {
                *reference_counts.entry(id).or_default() += 1;
            }
        }
    }

    let mut styles = Vec::<SvgGlyphStyle>::new();
    let mut style_ids = HashMap::<([u32; 4], String), u32>::new();
    let mut glyphs = Vec::<SvgGlyphUse>::new();
    let mut converted_references = HashMap::<String, usize>::new();
    let mut replacements = Vec::<Replacement>::new();
    let mut previous_x = 0i32;
    let mut previous_y = 0i32;

    for tag in &tags {
        if tag.name == "image"
            && svg[tag.start..tag.end].trim_end().ends_with("/>")
            && !inside_svg_resource(svg, tag.start)
        {
            let Some((_, image_uri)) = tag
                .attributes
                .iter()
                .find(|(name, _)| name == "href" || name == "xlink:href")
            else {
                continue;
            };
            let Some(encoded_image) = image_uri.strip_prefix(AVIF_DATA_URI_PREFIX) else {
                continue;
            };
            let image_bytes = STANDARD.decode(encoded_image).map_err(|error| {
                EncoderError::PdfIngest(format!("invalid AVIF image data URI in SVG: {error}"))
            })?;
            let image_hash = *blake3::hash(&image_bytes).as_bytes();
            let repeated = avif_asset_uses.get(&image_hash).copied().unwrap_or(0) > 1;
            if !repeated && !assets.contains_bytes(&image_bytes) {
                continue;
            }
            let asset_id = assets.insert(AssetInput::new(image_bytes, "image/avif"))?;
            let Some((_, start, end)) = tag
                .attribute_value_ranges
                .iter()
                .find(|(name, _, _)| name == "href" || name == "xlink:href")
            else {
                continue;
            };
            replacements.push(Replacement {
                start: *start,
                end: *end,
                value: format!("{ASSET_URI_PREFIX}{asset_id}"),
            });
            continue;
        }

        if tag.name == "use"
            && svg[tag.start..tag.end].trim_end().ends_with("/>")
            && !inside_svg_resource(svg, tag.start)
        {
            let Some(id) = use_reference(tag) else {
                continue;
            };
            let Some(definition) = path_definitions.get(&id) else {
                continue;
            };
            let attrs = attribute_map(tag);
            if attrs.len() != tag.attributes.len() {
                continue;
            }
            let Some(presentation) =
                presentation_attributes(tag, &["href", "xlink:href", "transform"])
            else {
                continue;
            };
            let Some(matrix) = attrs
                .get("transform")
                .map_or(Some(identity_matrix()), |value| parse_matrix(value))
            else {
                continue;
            };
            let Some((x, y)) = glyph_position(matrix, &definition.path) else {
                continue;
            };
            let glyph_matrix = matrix_for_path(matrix, &definition.path);
            let asset_id = match definition.asset_id {
                Some(asset_id) => asset_id,
                None => {
                    let asset_id = store_path_asset(&definition.path, assets)?;
                    path_definitions
                        .get_mut(&id)
                        .expect("path definition remains present")
                        .asset_id = Some(asset_id);
                    asset_id
                }
            };
            push_glyph(
                &mut styles,
                &mut style_ids,
                &mut glyphs,
                asset_id,
                glyph_matrix,
                presentation,
                x,
                y,
                &mut previous_x,
                &mut previous_y,
            )?;
            *converted_references.entry(id).or_default() += 1;
            replacements.push(Replacement {
                start: tag.start,
                end: tag.end,
                value: GLYPH_MARKER.into(),
            });
            continue;
        }

        if tag.name != "path"
            || !svg[tag.start..tag.end].trim_end().ends_with("/>")
            || inside_svg_resource(svg, tag.start)
            || is_inside(svg, tag.start, "defs")
        {
            continue;
        }
        let attrs = attribute_map(tag);
        if attrs.len() != tag.attributes.len() {
            continue;
        }
        let (Some(data), Some(presentation)) = (
            attrs.get("d"),
            presentation_attributes(tag, &["d", "transform"]),
        ) else {
            continue;
        };
        let Some(path) = crate::vector_path::encode_path_data_unscaled(data) else {
            continue;
        };
        let Some(matrix) = attrs
            .get("transform")
            .map_or(Some(identity_matrix()), |value| parse_matrix(value))
        else {
            continue;
        };
        let Some((x, y)) = glyph_position(matrix, &path) else {
            continue;
        };
        let glyph_matrix = matrix_for_path(matrix, &path);
        let (asset_bytes, media_type) = path_asset_bytes(&path.bytes)?;
        let already_present = assets.contains_bytes(&asset_bytes);
        let path_hash = *blake3::hash(&path.bytes).as_bytes();
        let repeated = direct_path_uses.get(&path_hash).copied().unwrap_or(0)
            >= DIRECT_PATH_ASSET_MIN_USES
            && path.bytes.len() >= DIRECT_PATH_ASSET_MIN_BYTES;
        if !already_present && !repeated {
            continue;
        }
        let asset_id = assets.insert(AssetInput::new(asset_bytes, media_type))?;
        push_glyph(
            &mut styles,
            &mut style_ids,
            &mut glyphs,
            asset_id,
            glyph_matrix,
            presentation,
            x,
            y,
            &mut previous_x,
            &mut previous_y,
        )?;
        replacements.push(Replacement {
            start: tag.start,
            end: tag.end,
            value: GLYPH_MARKER.into(),
        });
    }

    for (id, definition) in &path_definitions {
        let total_uses = reference_counts.get(id).copied().unwrap_or(0);
        let converted_uses = converted_references.get(id).copied().unwrap_or(0);
        if total_uses > 0 && total_uses == converted_uses {
            let tag = &tags[definition.tag_index];
            replacements.push(Replacement {
                start: tag.start,
                end: tag.end,
                value: String::new(),
            });
        }
    }

    if glyphs.is_empty() && replacements.is_empty() {
        return Ok(None);
    }
    replacements.sort_unstable_by_key(|replacement| replacement.start);
    let mut template = String::with_capacity(svg.len());
    let mut copied_until = 0usize;
    for replacement in replacements {
        if replacement.start < copied_until {
            return Ok(None);
        }
        template.push_str(&svg[copied_until..replacement.start]);
        template.push_str(&replacement.value);
        copied_until = replacement.end;
    }
    template.push_str(&svg[copied_until..]);

    Ok(Some(CompiledSvg {
        template,
        glyph_styles: styles,
        glyphs,
    }))
}

pub(crate) fn collect_direct_path_uses(svg: &str, counts: &mut HashMap<[u8; 32], usize>) {
    for tag in find_svg_tags(svg) {
        if tag.name != "path"
            || !svg[tag.start..tag.end].trim_end().ends_with("/>")
            || inside_svg_resource(svg, tag.start)
            || is_inside(svg, tag.start, "defs")
        {
            continue;
        }
        let attrs = attribute_map(&tag);
        if attrs.len() != tag.attributes.len()
            || presentation_attributes(&tag, &["d", "transform"]).is_none()
        {
            continue;
        }
        let Some(data) = attrs.get("d") else {
            continue;
        };
        let Some(path) = crate::vector_path::encode_path_data_unscaled(data) else {
            continue;
        };
        let Some(matrix) = attrs
            .get("transform")
            .map_or(Some(identity_matrix()), |value| parse_matrix(value))
        else {
            continue;
        };
        if glyph_position(matrix, &path).is_none() {
            continue;
        }
        let hash = *blake3::hash(&path.bytes).as_bytes();
        *counts.entry(hash).or_insert(0) += 1;
    }
}

pub(crate) fn collect_avif_asset_uses(
    svg: &str,
    counts: &mut HashMap<[u8; 32], usize>,
) -> Result<(), EncoderError> {
    for tag in find_svg_tags(svg) {
        if tag.name != "image" || !svg[tag.start..tag.end].trim_end().ends_with("/>") {
            continue;
        }
        let Some((_, image_uri)) = tag
            .attributes
            .iter()
            .find(|(name, _)| name == "href" || name == "xlink:href")
        else {
            continue;
        };
        let Some(encoded_image) = image_uri.strip_prefix(AVIF_DATA_URI_PREFIX) else {
            continue;
        };
        let image_bytes = STANDARD.decode(encoded_image).map_err(|error| {
            EncoderError::PdfIngest(format!("invalid AVIF image data URI in SVG: {error}"))
        })?;
        *counts
            .entry(*blake3::hash(&image_bytes).as_bytes())
            .or_insert(0) += 1;
    }
    Ok(())
}

fn find_svg_tags(svg: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = svg[cursor..].find('<') {
        let start = cursor + relative_start;
        let Some((end, name, attributes, attribute_value_ranges)) = parse_tag(svg, start) else {
            cursor = start + 1;
            continue;
        };
        cursor = end;
        if matches!(name.as_str(), "path" | "use" | "image") {
            tags.push(Tag {
                start,
                end,
                name,
                attributes,
                attribute_value_ranges,
            });
        }
    }
    tags
}

fn parse_tag(svg: &str, start: usize) -> Option<ParsedTag> {
    let bytes = svg.as_bytes();
    let mut cursor = start.checked_add(1)?;
    if *bytes.get(cursor)? == b'/' || *bytes.get(cursor)? == b'!' || *bytes.get(cursor)? == b'?' {
        let end = find_tag_end(bytes, cursor)?;
        return Some((end + 1, String::new(), Vec::new(), Vec::new()));
    }
    let name_start = cursor;
    while bytes.get(cursor).is_some_and(|byte| is_name_byte(*byte)) {
        cursor += 1;
    }
    if name_start == cursor {
        return None;
    }
    let name = svg[name_start..cursor].to_owned();
    let tag_end = find_tag_end(bytes, cursor)?;
    let mut attributes = Vec::new();
    let mut attribute_value_ranges = Vec::new();
    while cursor < tag_end {
        skip_space(bytes, &mut cursor);
        if cursor >= tag_end || bytes[cursor] == b'/' {
            break;
        }
        let attribute_start = cursor;
        while cursor < tag_end
            && !matches!(
                bytes[cursor],
                b'=' | b'/' | b'>' | b' ' | b'\t' | b'\r' | b'\n'
            )
        {
            cursor += 1;
        }
        if attribute_start == cursor {
            break;
        }
        let attribute_name = svg[attribute_start..cursor].to_owned();
        skip_space(bytes, &mut cursor);
        if cursor >= tag_end || bytes[cursor] != b'=' {
            return None;
        }
        cursor += 1;
        skip_space(bytes, &mut cursor);
        let quote = *bytes.get(cursor)?;
        if quote != b'\'' && quote != b'"' {
            return None;
        }
        cursor += 1;
        let value_start = cursor;
        while cursor < tag_end && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= tag_end {
            return None;
        }
        attributes.push((attribute_name.clone(), svg[value_start..cursor].to_owned()));
        attribute_value_ranges.push((attribute_name, value_start, cursor));
        cursor += 1;
    }
    Some((tag_end + 1, name, attributes, attribute_value_ranges))
}

fn find_tag_end(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        match (quote, byte) {
            (Some(open), value) if open == value => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(cursor),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')
}

fn skip_space(bytes: &[u8], cursor: &mut usize) {
    while bytes
        .get(*cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    {
        *cursor += 1;
    }
}

fn attribute_map(tag: &Tag) -> HashMap<String, String> {
    tag.attributes.iter().cloned().collect()
}

fn is_inside(svg: &str, position: usize, element: &str) -> bool {
    let prefix = &svg[..position];
    let open = prefix.rfind(&format!("<{element}"));
    let close = prefix.rfind(&format!("</{element}>"));
    open.is_some_and(|open| close.is_none_or(|close| open > close))
}

fn inside_svg_resource(svg: &str, position: usize) -> bool {
    ["defs", "clipPath", "mask", "pattern", "marker", "symbol"]
        .iter()
        .any(|element| is_inside(svg, position, element))
}

fn use_reference(tag: &Tag) -> Option<String> {
    let href = tag
        .attributes
        .iter()
        .find(|(name, _)| name == "href" || name == "xlink:href")?
        .1
        .as_str();
    href.strip_prefix('#').map(str::to_owned)
}

fn presentation_attributes(tag: &Tag, ignored: &[&str]) -> Option<String> {
    let mut presentation = String::new();
    for (name, value) in &tag.attributes {
        if ignored.contains(&name.as_str()) {
            continue;
        }
        if !PRESENTATION_ATTRIBUTES.contains(&name.as_str()) {
            return None;
        }
        if name == "style" && value.to_ascii_lowercase().contains("transform:") {
            return None;
        }
        if !presentation.is_empty() {
            presentation.push(' ');
        }
        presentation.push_str(name);
        presentation.push_str("=\"");
        presentation.push_str(&escape_attribute(value));
        presentation.push('"');
    }
    Some(presentation)
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn glyph_position(matrix: Matrix, path: &EncodedPath) -> Option<(i32, i32)> {
    let origin_x = (i64::from(path.origin_x) + i64::from(path.min_x)) as f32 / 64.0;
    let origin_y = (i64::from(path.origin_y) + i64::from(path.min_y)) as f32 / 64.0;
    let x = matrix.e + matrix.a * origin_x + matrix.c * origin_y;
    let y = matrix.f + matrix.b * origin_x + matrix.d * origin_y;
    Some((quantize_page_point(x)?, quantize_page_point(y)?))
}

fn matrix_for_path(matrix: Matrix, path: &EncodedPath) -> Matrix {
    let scale = path.scale as f32;
    Matrix {
        a: matrix.a * scale,
        b: matrix.b * scale,
        c: matrix.c * scale,
        d: matrix.d * scale,
        e: matrix.e,
        f: matrix.f,
    }
}

fn store_path_asset(path: &EncodedPath, assets: &mut AssetPool) -> Result<u32, EncoderError> {
    let (bytes, media_type) = path_asset_bytes(&path.bytes)?;
    assets.insert(AssetInput::new(bytes, media_type))
}

fn path_asset_bytes(path: &[u8]) -> Result<(Vec<u8>, &'static str), EncoderError> {
    let compressed = zstd::bulk::compress(path, 9)?;
    if compressed.len() < path.len() {
        Ok((compressed, VECTOR_PATH_ZSTD_MEDIA_TYPE))
    } else {
        Ok((path.to_vec(), VECTOR_PATH_MEDIA_TYPE))
    }
}

#[allow(clippy::too_many_arguments)]
fn push_glyph(
    styles: &mut Vec<SvgGlyphStyle>,
    style_ids: &mut HashMap<([u32; 4], String), u32>,
    glyphs: &mut Vec<SvgGlyphUse>,
    asset_id: u32,
    matrix: Matrix,
    presentation: String,
    x: i32,
    y: i32,
    previous_x: &mut i32,
    previous_y: &mut i32,
) -> Result<(), EncoderError> {
    let style = SvgGlyphStyle {
        matrix: [matrix.a, matrix.b, matrix.c, matrix.d],
        presentation,
    };
    let style_key = (style.matrix.map(f32::to_bits), style.presentation.clone());
    let style_id = match style_ids.get(&style_key).copied() {
        Some(style_id) => style_id,
        None => {
            let style_id = u32::try_from(styles.len())
                .map_err(|_| EncoderError::InvalidOption("too many SVG glyph styles"))?;
            styles.push(style);
            style_ids.insert(style_key, style_id);
            style_id
        }
    };
    glyphs.push(SvgGlyphUse {
        asset_id,
        style_id,
        dx: x
            .checked_sub(*previous_x)
            .ok_or(EncoderError::InvalidOption("SVG glyph x delta exceeds i32"))?,
        dy: y
            .checked_sub(*previous_y)
            .ok_or(EncoderError::InvalidOption("SVG glyph y delta exceeds i32"))?,
    });
    *previous_x = x;
    *previous_y = y;
    Ok(())
}

fn identity_matrix() -> Matrix {
    Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    }
}

fn parse_matrix(input: &str) -> Option<Matrix> {
    let trimmed = input.trim();
    let parameters = trimmed.strip_prefix("matrix(")?.strip_suffix(')')?;
    let values = parameters
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let [a, b, c, d, e, f] = values.as_slice() else {
        return None;
    };
    let matrix = Matrix {
        a: *a,
        b: *b,
        c: *c,
        d: *d,
        e: *e,
        f: *f,
    };
    [matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f]
        .into_iter()
        .all(f32::is_finite)
        .then_some(matrix)
}

fn quantize_page_point(value: f32) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let scaled = (f64::from(value) * 64.0).round();
    if scaled < f64::from(i32::MIN) || scaled > f64::from(i32::MAX) {
        return None;
    }
    Some(scaled as i32)
}

#[cfg(test)]
mod tests {
    use crate::{asset_pool::AssetPool, svg_compiler::compile_svg};

    #[test]
    fn glyph_paths_become_reused_assets_and_small_template_markers() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg"><use fill="#000" transform="matrix(1 0 0 1 10 20)" xlink:href="#g0"/><use fill="#000" transform="matrix(1 0 0 1 12 20)" xlink:href="#g0"/><defs><path d="M10 20L11 20L11 21Z" id="g0"/></defs></svg>"##;
        let mut assets = AssetPool::default();
        let compiled = compile_svg(svg, &mut assets).unwrap().unwrap();

        assert_eq!(assets.entries().len(), 1);
        assert_eq!(compiled.glyphs.len(), 2);
        assert_eq!(compiled.glyphs[0].asset_id, compiled.glyphs[1].asset_id);
        assert_eq!(compiled.glyphs[0].dx, 1280);
        assert_eq!(compiled.glyphs[0].dy, 2560);
        assert_eq!(compiled.glyphs[1].dx, 128);
        assert_eq!(compiled.template.matches("<!--HDOC_GLYPH-->").count(), 2);
        assert!(!compiled.template.contains("<path d="));
    }

    #[test]
    fn avif_data_uris_move_to_deduplicated_binary_assets() {
        let svg = r##"<svg><image x="1" y="2" width="3" height="4" href="data:image/avif;base64,aGVsbG8="/><image x="5" y="6" width="7" height="8" href="data:image/avif;base64,aGVsbG8="/></svg>"##;
        let mut assets = AssetPool::default();
        let compiled = compile_svg(svg, &mut assets).unwrap().unwrap();

        assert!(compiled.glyphs.is_empty());
        assert_eq!(assets.entries().len(), 1);
        assert_eq!(assets.entries()[0].input.bytes, b"hello");
        assert_eq!(assets.entries()[0].input.media_type, "image/avif");
        assert_eq!(assets.duplicate_count(), 1);
        assert!(!compiled.template.contains("data:image/avif;base64,"));
        assert_eq!(compiled.template.matches("hdoc-asset://0").count(), 2);
    }

    #[test]
    fn unsupported_use_attributes_keep_the_original_svg_path() {
        let svg = r##"<svg><use data-custom="value" fill="#000" transform="matrix(1 0 0 1 0 0)" xlink:href="#g0"/><defs><path d="M0 0L1 1Z" id="g0"/></defs></svg>"##;
        let mut assets = AssetPool::default();
        assert!(compile_svg(svg, &mut assets).unwrap().is_none());
        assert!(assets.entries().is_empty());
    }

    #[test]
    fn heavily_repeated_direct_paths_become_binary_assets() {
        let path = r##"<path d="M0 0 C10 10 20 20 30 30 C40 40 50 50 60 60 C70 70 80 80 90 90 C100 100 110 110 120 120" fill="#fff" stroke="#000" stroke-width="2" transform="matrix(1 0 0 1 4 6)"/>"##;
        let svg = format!("<svg>{}</svg>", path.repeat(64));
        let mut assets = AssetPool::default();
        let compiled = compile_svg(&svg, &mut assets).unwrap().unwrap();

        assert_eq!(compiled.glyphs.len(), 64);
        assert_eq!(assets.entries().len(), 1);
        assert!(compiled.glyph_styles[0]
            .presentation
            .contains("stroke-width=\"2\""));
        assert_eq!(compiled.glyphs[0].dx, 256);
        assert_eq!(compiled.glyphs[0].dy, 384);
    }

    #[test]
    fn three_repeated_direct_paths_become_binary_assets() {
        let path = r##"<path d="M0 0 C10 10 20 20 30 30 C40 40 50 50 60 60 C70 70 80 80 90 90 C100 100 110 110 120 120" fill="#fff"/>"##;
        let svg = format!("<svg>{path}{path}{path}</svg>");
        let mut assets = AssetPool::default();
        let compiled = compile_svg(&svg, &mut assets).unwrap().unwrap();
        assert_eq!(compiled.glyphs.len(), 3);
        assert_eq!(assets.entries().len(), 1);
    }

    #[test]
    fn short_repeated_paths_stay_inline() {
        let path = r##"<path d="M0 0Z" fill="#fff"/>"##;
        let svg = format!("<svg>{path}{path}{path}</svg>");
        let mut assets = AssetPool::default();
        assert!(compile_svg(&svg, &mut assets).unwrap().is_none());
        assert!(assets.entries().is_empty());
    }

    #[test]
    fn unique_direct_paths_stay_in_the_compressed_svg_fallback() {
        let svg = r##"<svg><path d="M0 0 C10 10 20 20 30 30 C40 40 50 50 60 60 C70 70 80 80 90 90 C100 100 110 110 120 120" fill="#fff"/></svg>"##;
        let mut assets = AssetPool::default();
        assert!(compile_svg(svg, &mut assets).unwrap().is_none());
        assert!(assets.entries().is_empty());
    }
}
