use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    asset_pool::AssetPool, vector_path::EncodedPath, AssetInput, EncoderError, SvgGlyphStyle,
    SvgGlyphUse,
};

const GLYPH_MARKER: &str = "<!--HDOC_GLYPH-->";
const ASSET_URI_PREFIX: &str = "hdoc-asset://";
const DIRECT_PATH_ASSET_MIN_USES: usize = 3;
const DIRECT_PATH_ASSET_MIN_BYTES: usize = 14;
const VECTOR_PATH_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path;version=1";
const VECTOR_PATH_ZSTD_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path+zstd;version=1";
const INLINE_IMAGE_DATA_URIS: &[(&str, &str)] = &[
    ("data:image/avif;base64,", "image/avif"),
    ("data:image/png;base64,", "image/png"),
    ("data:image/jpeg;base64,", "image/jpeg"),
    ("data:image/jpg;base64,", "image/jpeg"),
    ("data:image/webp;base64,", "image/webp"),
];
const RESOURCE_ELEMENTS: &[&str] = &["defs", "clipPath", "mask", "pattern", "marker", "symbol"];
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
    self_closing: bool,
    in_defs: bool,
    in_resource: bool,
    attributes: Vec<(String, String)>,
    attribute_value_ranges: Vec<(String, usize, usize)>,
}

#[derive(Debug, Clone, Copy)]
enum RawTagKind {
    Open { self_closing: bool },
    Close,
    Declaration,
}

type ParsedTag = (
    usize,
    RawTagKind,
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
    compile_svg_with_usage(svg, assets, &direct_path_uses)
}

pub(crate) fn compile_svg_with_usage(
    svg: &str,
    assets: &mut AssetPool,
    direct_path_uses: &HashMap<[u8; 32], usize>,
) -> Result<Option<CompiledSvg>, EncoderError> {
    let tags = find_svg_tags(svg);
    let mut path_definitions = HashMap::<String, PathDefinition>::new();

    for (tag_index, tag) in tags.iter().enumerate() {
        if tag.name != "path" || !tag.self_closing || !tag.in_defs {
            continue;
        }
        let attrs = attribute_map(tag);
        let (Some(id), Some(data)) = (attrs.get("id"), attrs.get("d")) else {
            continue;
        };
        if attrs.len() != 2 {
            continue;
        }
        let Some(path) = crate::vector_path::encode_path_data(data) else {
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
        if tag.name != "use" || !tag.self_closing {
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
        if tag.name == "image" && tag.self_closing && !tag.in_resource {
            let Some((_, image_uri)) = tag
                .attributes
                .iter()
                .find(|(name, _)| name == "href" || name == "xlink:href")
            else {
                continue;
            };
            let Some((encoded_image, media_type)) = strip_inline_image_uri(image_uri) else {
                continue;
            };
            let image_bytes = STANDARD.decode(encoded_image).map_err(|error| {
                EncoderError::PdfIngest(format!("invalid inline image data URI in SVG: {error}"))
            })?;
            let (width, height) = parse_image_tag_dimensions(tag);
            let asset_id = assets
                .insert(AssetInput::new(image_bytes, media_type).with_dimensions(width, height))?;
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

        if tag.name == "use" && tag.self_closing && !tag.in_resource {
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
                .map_or(Some(identity_matrix()), |value| parse_svg_transform(value))
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

        if tag.name != "path" || !tag.self_closing || tag.in_resource {
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
        let Some(path) = crate::vector_path::encode_path_data(data) else {
            continue;
        };
        let Some(matrix) = attrs
            .get("transform")
            .map_or(Some(identity_matrix()), |value| parse_svg_transform(value))
        else {
            continue;
        };
        let Some((x, y)) = glyph_position(matrix, &path) else {
            continue;
        };
        let glyph_matrix = matrix_for_path(matrix, &path);
        let path_hash = *blake3::hash(&path.bytes).as_bytes();
        let repeated = direct_path_uses.get(&path_hash).copied().unwrap_or(0)
            >= DIRECT_PATH_ASSET_MIN_USES
            && path.bytes.len() >= DIRECT_PATH_ASSET_MIN_BYTES;
        if !repeated {
            continue;
        }
        let (asset_bytes, media_type) = path_asset_bytes(&path.bytes)?;
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
    let template = strip_empty_defs(&template);

    Ok(Some(CompiledSvg {
        template,
        glyph_styles: styles,
        glyphs,
    }))
}

pub(crate) fn collect_direct_path_uses(svg: &str, counts: &mut HashMap<[u8; 32], usize>) {
    for tag in find_svg_tags(svg) {
        if tag.name != "path" || !tag.self_closing || tag.in_resource {
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
        let Some(path) = crate::vector_path::encode_path_data(data) else {
            continue;
        };
        let Some(matrix) = attrs
            .get("transform")
            .map_or(Some(identity_matrix()), |value| parse_svg_transform(value))
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

fn strip_inline_image_uri(uri: &str) -> Option<(&str, &'static str)> {
    let trimmed = uri.trim();
    for (prefix, media_type) in INLINE_IMAGE_DATA_URIS {
        if let Some(payload) = trimmed.strip_prefix(prefix) {
            return Some((payload, *media_type));
        }
    }
    None
}

fn parse_image_tag_dimensions(tag: &Tag) -> (u32, u32) {
    let mut width = 0u32;
    let mut height = 0u32;
    for (name, value) in &tag.attributes {
        if name == "width" {
            width = parse_positive_dimension(value).unwrap_or(0);
        } else if name == "height" {
            height = parse_positive_dimension(value).unwrap_or(0);
        }
    }
    (width, height)
}

fn parse_positive_dimension(value: &str) -> Option<u32> {
    let trimmed = value.trim().trim_end_matches("px").trim();
    let parsed = trimmed.parse::<f64>().ok()?;
    if !parsed.is_finite() || parsed <= 0.0 || parsed > f64::from(u32::MAX) {
        return None;
    }
    Some(parsed.round() as u32)
}

fn strip_empty_defs(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(open_pos) = rest.find("<defs>") {
        let after_open = &rest[open_pos + "<defs>".len()..];
        if let Some(close_rel) = after_open.find("</defs>") {
            if after_open[..close_rel].chars().all(char::is_whitespace) {
                out.push_str(&rest[..open_pos]);
                rest = &after_open[close_rel + "</defs>".len()..];
                continue;
            }
        }
        out.push_str(&rest[..open_pos + "<defs>".len()]);
        rest = after_open;
    }
    out.push_str(rest);
    out
}

fn find_svg_tags(svg: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = svg[cursor..].find('<') {
        let start = cursor + relative_start;
        let Some((end, kind, name, attributes, attribute_value_ranges)) = parse_tag(svg, start)
        else {
            cursor = start + 1;
            continue;
        };
        cursor = end;
        match kind {
            RawTagKind::Declaration => continue,
            RawTagKind::Close => {
                if let Some(pos) = stack.iter().rposition(|open| open == &name) {
                    stack.truncate(pos);
                }
            }
            RawTagKind::Open { self_closing } => {
                let in_defs = stack.iter().any(|open| open == "defs");
                let in_resource = stack
                    .iter()
                    .any(|open| RESOURCE_ELEMENTS.contains(&open.as_str()));
                if matches!(name.as_str(), "path" | "use" | "image") {
                    tags.push(Tag {
                        start,
                        end,
                        name: name.clone(),
                        self_closing,
                        in_defs,
                        in_resource,
                        attributes,
                        attribute_value_ranges,
                    });
                }
                if !self_closing {
                    stack.push(name);
                }
            }
        }
    }
    tags
}

fn parse_tag(svg: &str, start: usize) -> Option<ParsedTag> {
    let bytes = svg.as_bytes();
    if bytes.get(start..start + 4) == Some(b"<!--") {
        let comment_end = svg[start + 4..].find("-->")?;
        return Some((
            start + 4 + comment_end + 3,
            RawTagKind::Declaration,
            String::new(),
            Vec::new(),
            Vec::new(),
        ));
    }
    if bytes.get(start..start + 9) == Some(b"<![CDATA[") {
        let cdata_end = svg[start + 9..].find("]]>")?;
        return Some((
            start + 9 + cdata_end + 3,
            RawTagKind::Declaration,
            String::new(),
            Vec::new(),
            Vec::new(),
        ));
    }
    let mut cursor = start.checked_add(1)?;
    let first = *bytes.get(cursor)?;
    if first == b'!' || first == b'?' {
        let end = find_tag_end(bytes, cursor)?;
        return Some((
            end + 1,
            RawTagKind::Declaration,
            String::new(),
            Vec::new(),
            Vec::new(),
        ));
    }
    if first == b'/' {
        cursor += 1;
        skip_space(bytes, &mut cursor);
        let name_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| is_name_byte(*byte)) {
            cursor += 1;
        }
        let name = svg[name_start..cursor].to_owned();
        let end = find_tag_end(bytes, cursor)?;
        return Some((end + 1, RawTagKind::Close, name, Vec::new(), Vec::new()));
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
    let mut self_closing = false;
    while cursor < tag_end {
        skip_space(bytes, &mut cursor);
        if cursor >= tag_end {
            break;
        }
        if bytes[cursor] == b'/' {
            self_closing = true;
            cursor += 1;
            skip_space(bytes, &mut cursor);
            if cursor != tag_end {
                return None;
            }
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
    Some((
        tag_end + 1,
        RawTagKind::Open { self_closing },
        name,
        attributes,
        attribute_value_ranges,
    ))
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
        if name == "style" {
            let lower = value.to_ascii_lowercase();
            if lower.contains("transform:")
                || lower.contains("javascript:")
                || lower.contains("expression(")
                || lower.contains("@import")
            {
                return None;
            }
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

fn multiply_matrices(left: Matrix, right: Matrix) -> Matrix {
    Matrix {
        a: left.a * right.a + left.c * right.b,
        b: left.b * right.a + left.d * right.b,
        c: left.a * right.c + left.c * right.d,
        d: left.b * right.c + left.d * right.d,
        e: left.a * right.e + left.c * right.f + left.e,
        f: left.b * right.e + left.d * right.f + left.f,
    }
}

/// Parses full SVG transform lists (`matrix`, `translate`, `scale`, `rotate`,
/// `skewX`, `skewY`) into a single 2D affine matrix.
fn parse_svg_transform(input: &str) -> Option<Matrix> {
    let mut rest = input.trim();
    if rest.is_empty() {
        return Some(identity_matrix());
    }
    let mut combined = identity_matrix();
    let mut parsed_any = false;
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace() || ch == ',');
        if rest.is_empty() {
            break;
        }
        let open_paren = rest.find('(')?;
        let kind = rest[..open_paren].trim();
        let after_open = &rest[open_paren + 1..];
        let close_paren = after_open.find(')')?;
        let args = after_open[..close_paren]
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .filter(|token| !token.is_empty())
            .map(str::parse::<f32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let step = match (kind, args.as_slice()) {
            ("matrix", [a, b, c, d, e, f]) => Matrix {
                a: *a,
                b: *b,
                c: *c,
                d: *d,
                e: *e,
                f: *f,
            },
            ("translate", [tx]) => Matrix {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: *tx,
                f: 0.0,
            },
            ("translate", [tx, ty]) => Matrix {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: *tx,
                f: *ty,
            },
            ("scale", [sx]) => Matrix {
                a: *sx,
                b: 0.0,
                c: 0.0,
                d: *sx,
                e: 0.0,
                f: 0.0,
            },
            ("scale", [sx, sy]) => Matrix {
                a: *sx,
                b: 0.0,
                c: 0.0,
                d: *sy,
                e: 0.0,
                f: 0.0,
            },
            ("rotate", [deg]) => {
                let rad = deg.to_radians();
                let (sin, cos) = rad.sin_cos();
                Matrix {
                    a: cos,
                    b: sin,
                    c: -sin,
                    d: cos,
                    e: 0.0,
                    f: 0.0,
                }
            }
            ("rotate", [deg, cx, cy]) => {
                let rad = deg.to_radians();
                let (sin, cos) = rad.sin_cos();
                let to_origin = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: *cx,
                    f: *cy,
                };
                let rot = Matrix {
                    a: cos,
                    b: sin,
                    c: -sin,
                    d: cos,
                    e: 0.0,
                    f: 0.0,
                };
                let back = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: -*cx,
                    f: -*cy,
                };
                multiply_matrices(multiply_matrices(to_origin, rot), back)
            }
            ("skewX", [deg]) => Matrix {
                a: 1.0,
                b: 0.0,
                c: deg.to_radians().tan(),
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            ("skewY", [deg]) => Matrix {
                a: 1.0,
                b: deg.to_radians().tan(),
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            _ => return None,
        };
        combined = multiply_matrices(combined, step);
        parsed_any = true;
        rest = &after_open[close_paren + 1..];
    }
    if !parsed_any {
        return None;
    }
    [
        combined.a, combined.b, combined.c, combined.d, combined.e, combined.f,
    ]
    .into_iter()
    .all(f32::is_finite)
    .then_some(combined)
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
        assert!(!compiled.template.contains("<defs></defs>"));
    }

    #[test]
    fn self_closing_defs_or_clip_paths_do_not_poison_following_elements() {
        let svg = r##"<svg><defs/><clipPath id="c0"/><defs><path d="M0 0L4 0L4 4Z" id="g0"/></defs><use fill="#111" transform="translate(10, 20) scale(2)" href="#g0"/></svg>"##;
        let mut assets = AssetPool::default();
        let compiled = compile_svg(svg, &mut assets).unwrap().unwrap();

        assert_eq!(assets.entries().len(), 1);
        assert_eq!(compiled.glyphs.len(), 1);
        assert_eq!(compiled.glyphs[0].dx, 640);
        assert_eq!(compiled.glyphs[0].dy, 1280);
        assert!(compiled.template.contains("<!--HDOC_GLYPH-->"));
    }

    #[test]
    fn single_and_repeated_inline_images_move_to_binary_assets() {
        let svg = r##"<svg><image x="1" y="2" width="320" height="240" href="data:image/avif;base64,aGVsbG8="/><image x="5" y="6" width="320" height="240" href="data:image/avif;base64,aGVsbG8="/><image x="0" y="0" width="64" height="48" href="data:image/png;base64,cG5n"/></svg>"##;
        let mut assets = AssetPool::default();
        let compiled = compile_svg(svg, &mut assets).unwrap().unwrap();

        assert!(compiled.glyphs.is_empty());
        assert_eq!(assets.entries().len(), 2);
        assert_eq!(assets.entries()[0].input.bytes, b"hello");
        assert_eq!(assets.entries()[0].input.media_type, "image/avif");
        assert_eq!(assets.entries()[0].input.width, 320);
        assert_eq!(assets.entries()[0].input.height, 240);
        assert_eq!(assets.entries()[1].input.bytes, b"png");
        assert_eq!(assets.entries()[1].input.media_type, "image/png");
        assert_eq!(assets.duplicate_count(), 1);
        assert!(!compiled.template.contains("base64,"));
        assert_eq!(compiled.template.matches("hdoc-asset://0").count(), 2);
        assert_eq!(compiled.template.matches("hdoc-asset://1").count(), 1);
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
    fn proportionally_scaled_direct_paths_share_one_asset() {
        let p1 = r##"<path d="M0 0 C10 10 20 20 30 30 C40 40 50 50 60 60" fill="#fff"/>"##;
        let p2 = r##"<path d="M0 0 C20 20 40 40 60 60 C80 80 100 100 120 120" fill="#fff"/>"##;
        let p3 =
            r##"<path d="M10 10 C40 40 70 70 100 100 C130 130 160 160 190 190" fill="#fff"/>"##;
        let svg = format!("<svg>{p1}{p2}{p3}</svg>");
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
