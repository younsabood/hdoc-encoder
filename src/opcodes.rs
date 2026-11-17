use crate::{varint, HdocError};

#[derive(Debug, Clone, PartialEq)]
pub enum PageCommand {
    SetFont {
        asset_id: u32,
        size_fixed: u16,
    },
    SetColor {
        rgba: u32,
    },
    MoveAbs {
        x: i32,
        y: i32,
    },
    MoveDelta {
        dx: i32,
        dy: i32,
    },
    DrawGlyphRun {
        glyph_ids: Vec<u16>,
        offsets: Vec<GlyphOffset>,
    },
    DrawImage {
        asset_id: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    DrawVector {
        asset_id: u32,
        transform: [f32; 6],
    },
    DrawSvgFallback {
        svg: String,
    },
    DrawSvgTemplate {
        svg: String,
        glyph_styles: Vec<SvgGlyphStyle>,
        glyphs: Vec<SvgGlyphUse>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgGlyphStyle {
    pub matrix: [f32; 4],
    pub presentation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgGlyphUse {
    pub asset_id: u32,
    pub style_id: u32,
    pub dx: i32,
    pub dy: i32,
}

/// Signed glyph displacement in 1/64-point units, relative to the current pen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphOffset {
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageDisplayList {
    pub width: u16,
    pub height: u16,
    pub flags: u32,
    pub commands: Vec<PageCommand>,
}

impl PageDisplayList {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            flags: 0,
            commands: Vec::new(),
        }
    }

    pub fn push(&mut self, command: PageCommand) {
        self.commands.push(command);
    }

    pub fn draw_glyph_run(
        &mut self,
        glyph_ids: Vec<u16>,
        offsets: Vec<GlyphOffset>,
    ) -> Result<(), HdocError> {
        if glyph_ids.len() != offsets.len() {
            return Err(HdocError::InvalidFormat(
                "glyph IDs and offsets must have equal lengths".into(),
            ));
        }
        self.push(PageCommand::DrawGlyphRun { glyph_ids, offsets });
        Ok(())
    }
}

/// Encode one page's commands and append its mandatory END_PAGE opcode.
pub fn encode_page(page: &PageDisplayList) -> Result<Vec<u8>, HdocError> {
    let mut out = Vec::new();
    for command in &page.commands {
        match command {
            PageCommand::SetFont {
                asset_id,
                size_fixed,
            } => {
                out.push(0x01);
                varint::write_u32(*asset_id, &mut out);
                out.extend_from_slice(&size_fixed.to_le_bytes());
            }
            PageCommand::SetColor { rgba } => {
                out.push(0x02);
                out.extend_from_slice(&rgba.to_le_bytes());
            }
            PageCommand::MoveAbs { x, y } => {
                out.push(0x03);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            PageCommand::MoveDelta { dx, dy } => {
                out.push(0x04);
                varint::write_i32(*dx, &mut out);
                varint::write_i32(*dy, &mut out);
            }
            PageCommand::DrawGlyphRun { glyph_ids, offsets } => {
                if glyph_ids.len() != offsets.len() {
                    return Err(HdocError::InvalidFormat(
                        "glyph IDs and offsets must have equal lengths".into(),
                    ));
                }
                out.push(0x05);
                varint::write_u32(
                    u32::try_from(glyph_ids.len()).map_err(|_| HdocError::LimitExceeded)?,
                    &mut out,
                );
                for (glyph_id, offset) in glyph_ids.iter().zip(offsets) {
                    out.extend_from_slice(&glyph_id.to_le_bytes());
                    varint::write_i32(offset.dx, &mut out);
                    varint::write_i32(offset.dy, &mut out);
                }
            }
            PageCommand::DrawImage {
                asset_id,
                x,
                y,
                width,
                height,
            } => {
                out.push(0x06);
                varint::write_u32(*asset_id, &mut out);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
            }
            PageCommand::DrawVector {
                asset_id,
                transform,
            } => {
                out.push(0x07);
                varint::write_u32(*asset_id, &mut out);
                for value in transform {
                    if !value.is_finite() {
                        return Err(HdocError::InvalidFormat(
                            "vector transform must be finite".into(),
                        ));
                    }
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            PageCommand::DrawSvgFallback { svg } => {
                out.push(0x09);
                varint::write_u32(
                    u32::try_from(svg.len()).map_err(|_| HdocError::LimitExceeded)?,
                    &mut out,
                );
                out.extend_from_slice(svg.as_bytes());
            }
            PageCommand::DrawSvgTemplate {
                svg,
                glyph_styles,
                glyphs,
            } => {
                out.push(0x0a);
                varint::write_u32(
                    u32::try_from(svg.len()).map_err(|_| HdocError::LimitExceeded)?,
                    &mut out,
                );
                out.extend_from_slice(svg.as_bytes());
                varint::write_u32(
                    u32::try_from(glyph_styles.len()).map_err(|_| HdocError::LimitExceeded)?,
                    &mut out,
                );
                for style in glyph_styles {
                    for value in style.matrix {
                        if !value.is_finite() {
                            return Err(HdocError::InvalidFormat(
                                "SVG glyph transform must be finite".into(),
                            ));
                        }
                        out.extend_from_slice(&value.to_le_bytes());
                    }
                    varint::write_u32(
                        u32::try_from(style.presentation.len())
                            .map_err(|_| HdocError::LimitExceeded)?,
                        &mut out,
                    );
                    out.extend_from_slice(style.presentation.as_bytes());
                }
                varint::write_u32(
                    u32::try_from(glyphs.len()).map_err(|_| HdocError::LimitExceeded)?,
                    &mut out,
                );
                for glyph in glyphs {
                    if glyph.style_id as usize >= glyph_styles.len() {
                        return Err(HdocError::InvalidFormat(
                            "SVG glyph references an unknown style".into(),
                        ));
                    }
                    varint::write_u32(glyph.asset_id, &mut out);
                    varint::write_u32(glyph.style_id, &mut out);
                    varint::write_i32(glyph.dx, &mut out);
                    varint::write_i32(glyph.dy, &mut out);
                }
            }
        }
    }
    out.push(0x08);
    Ok(out)
}

/// Decode a page beginning at `input[0]`; returns the number of consumed bytes.
pub fn decode_page(
    input: &[u8],
    width: u16,
    height: u16,
    flags: u32,
) -> Result<(PageDisplayList, usize), HdocError> {
    let mut page = PageDisplayList {
        width,
        height,
        flags,
        commands: Vec::new(),
    };
    let mut cursor = 0usize;
    loop {
        let opcode = *input.get(cursor).ok_or(HdocError::Truncated)?;
        cursor += 1;
        match opcode {
            0x01 => {
                let asset_id = varint::read_u32(input, &mut cursor)?;
                let size_fixed = read_u16(input, &mut cursor)?;
                page.push(PageCommand::SetFont {
                    asset_id,
                    size_fixed,
                });
            }
            0x02 => page.push(PageCommand::SetColor {
                rgba: read_u32(input, &mut cursor)?,
            }),
            0x03 => page.push(PageCommand::MoveAbs {
                x: read_i32(input, &mut cursor)?,
                y: read_i32(input, &mut cursor)?,
            }),
            0x04 => page.push(PageCommand::MoveDelta {
                dx: varint::read_i32(input, &mut cursor)?,
                dy: varint::read_i32(input, &mut cursor)?,
            }),
            0x05 => {
                let count = varint::read_u32(input, &mut cursor)? as usize;
                if count > input.len().saturating_sub(cursor) / 4 {
                    return Err(HdocError::Truncated);
                }
                let mut glyph_ids = Vec::with_capacity(count);
                let mut offsets = Vec::with_capacity(count);
                for _ in 0..count {
                    glyph_ids.push(read_u16(input, &mut cursor)?);
                    offsets.push(GlyphOffset {
                        dx: varint::read_i32(input, &mut cursor)?,
                        dy: varint::read_i32(input, &mut cursor)?,
                    });
                }
                page.push(PageCommand::DrawGlyphRun { glyph_ids, offsets });
            }
            0x06 => page.push(PageCommand::DrawImage {
                asset_id: varint::read_u32(input, &mut cursor)?,
                x: read_i32(input, &mut cursor)?,
                y: read_i32(input, &mut cursor)?,
                width: read_u32(input, &mut cursor)?,
                height: read_u32(input, &mut cursor)?,
            }),
            0x07 => {
                let asset_id = varint::read_u32(input, &mut cursor)?;
                let mut transform = [0.0f32; 6];
                for value in &mut transform {
                    *value = f32::from_le_bytes(
                        take(input, &mut cursor, 4)?
                            .try_into()
                            .expect("fixed float slice"),
                    );
                    if !value.is_finite() {
                        return Err(HdocError::InvalidFormat(
                            "vector transform must be finite".into(),
                        ));
                    }
                }
                page.push(PageCommand::DrawVector {
                    asset_id,
                    transform,
                });
            }
            0x08 => return Ok((page, cursor)),
            0x09 => {
                let length = varint::read_u32(input, &mut cursor)? as usize;
                let svg = String::from_utf8(take(input, &mut cursor, length)?.to_vec())
                    .map_err(|_| HdocError::InvalidUtf8)?;
                page.push(PageCommand::DrawSvgFallback { svg });
            }
            0x0a => {
                let svg_length = varint::read_u32(input, &mut cursor)? as usize;
                let svg = String::from_utf8(take(input, &mut cursor, svg_length)?.to_vec())
                    .map_err(|_| HdocError::InvalidUtf8)?;
                let style_count = varint::read_u32(input, &mut cursor)? as usize;
                if style_count > input.len().saturating_sub(cursor) / 17 {
                    return Err(HdocError::Truncated);
                }
                let mut glyph_styles = Vec::with_capacity(style_count);
                for _ in 0..style_count {
                    let mut matrix = [0.0f32; 4];
                    for value in &mut matrix {
                        *value = f32::from_le_bytes(
                            take(input, &mut cursor, 4)?
                                .try_into()
                                .expect("fixed float slice"),
                        );
                        if !value.is_finite() {
                            return Err(HdocError::InvalidFormat(
                                "SVG glyph transform must be finite".into(),
                            ));
                        }
                    }
                    let presentation_len = varint::read_u32(input, &mut cursor)? as usize;
                    let presentation =
                        String::from_utf8(take(input, &mut cursor, presentation_len)?.to_vec())
                            .map_err(|_| HdocError::InvalidUtf8)?;
                    glyph_styles.push(SvgGlyphStyle {
                        matrix,
                        presentation,
                    });
                }
                let glyph_count = varint::read_u32(input, &mut cursor)? as usize;
                if glyph_count > input.len().saturating_sub(cursor) / 4 {
                    return Err(HdocError::Truncated);
                }
                let mut glyphs = Vec::with_capacity(glyph_count);
                for _ in 0..glyph_count {
                    let glyph = SvgGlyphUse {
                        asset_id: varint::read_u32(input, &mut cursor)?,
                        style_id: varint::read_u32(input, &mut cursor)?,
                        dx: varint::read_i32(input, &mut cursor)?,
                        dy: varint::read_i32(input, &mut cursor)?,
                    };
                    if glyph.style_id as usize >= glyph_styles.len() {
                        return Err(HdocError::InvalidFormat(
                            "SVG glyph references an unknown style".into(),
                        ));
                    }
                    glyphs.push(glyph);
                }
                let marker_count = svg.matches("<!--HDOC_GLYPH-->").count();
                if marker_count != glyphs.len() {
                    return Err(HdocError::InvalidFormat(
                        "SVG glyph template marker count does not match glyph table".into(),
                    ));
                }
                page.push(PageCommand::DrawSvgTemplate {
                    svg,
                    glyph_styles,
                    glyphs,
                });
            }
            other => return Err(HdocError::UnknownOpcode(other)),
        }
    }
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8], HdocError> {
    let end = cursor.checked_add(count).ok_or(HdocError::LimitExceeded)?;
    let bytes = input.get(*cursor..end).ok_or(HdocError::Truncated)?;
    *cursor = end;
    Ok(bytes)
}
fn read_u16(input: &[u8], cursor: &mut usize) -> Result<u16, HdocError> {
    Ok(u16::from_le_bytes(
        take(input, cursor, 2)?
            .try_into()
            .expect("fixed integer slice"),
    ))
}
fn read_u32(input: &[u8], cursor: &mut usize) -> Result<u32, HdocError> {
    Ok(u32::from_le_bytes(
        take(input, cursor, 4)?
            .try_into()
            .expect("fixed integer slice"),
    ))
}
fn read_i32(input: &[u8], cursor: &mut usize) -> Result<i32, HdocError> {
    Ok(i32::from_le_bytes(
        take(input, cursor, 4)?
            .try_into()
            .expect("fixed integer slice"),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_page, encode_page, GlyphOffset, PageCommand, PageDisplayList, SvgGlyphStyle,
        SvgGlyphUse,
    };

    #[test]
    fn glyph_runs_and_svg_fallback_round_trip_without_plain_text_opcodes() {
        let mut page = PageDisplayList::new(612, 792);
        page.push(PageCommand::SetFont {
            asset_id: 2,
            size_fixed: 768,
        });
        page.draw_glyph_run(
            vec![0x0644, 0x0627, 0x0645],
            vec![
                GlyphOffset { dx: -2, dy: 0 },
                GlyphOffset { dx: 18, dy: 0 },
                GlyphOffset { dx: 21, dy: -1 },
            ],
        )
        .unwrap();
        page.push(PageCommand::DrawSvgFallback {
            svg: "<svg/>".into(),
        });
        page.push(PageCommand::DrawSvgTemplate {
            svg: "<svg><!--HDOC_GLYPH--></svg>".into(),
            glyph_styles: vec![SvgGlyphStyle {
                matrix: [0.012, 0.0, 0.0, -0.012],
                presentation: "fill=\"#1895ad\"".into(),
            }],
            glyphs: vec![SvgGlyphUse {
                asset_id: 3,
                style_id: 0,
                dx: 64,
                dy: -128,
            }],
        });
        let encoded = encode_page(&page).unwrap();
        assert!(!encoded
            .windows("مرحبا".len())
            .any(|window| window == "مرحبا".as_bytes()));
        let (decoded, consumed) = decode_page(&encoded, 612, 792, 0).unwrap();
        assert_eq!(decoded, page);
        assert_eq!(consumed, encoded.len());
    }
}
