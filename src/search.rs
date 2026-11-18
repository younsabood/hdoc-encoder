use std::collections::BTreeMap;

use crate::{varint, HdocError};

const SEARCH_MAGIC: [u8; 4] = *b"HDSR";
const SEARCH_VERSION: u16 = 0x0100;
const SEARCH_FLAG_COMPACT_TF: u16 = 1 << 0;

/// Search data is stored separately from drawing opcodes so shaped text can
/// use glyph IDs without losing its original UTF-8 representation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchBlock {
    pub page_texts: Vec<String>,
    pub postings: BTreeMap<String, Vec<u32>>,
    pub term_frequencies: BTreeMap<String, Vec<(u32, u32)>>,
}

impl SearchBlock {
    pub fn from_page_texts(page_texts: Vec<String>) -> Self {
        let (postings, term_frequencies) =
            build_index_with_frequencies(page_texts.iter().map(String::as_str));
        Self {
            page_texts,
            postings,
            term_frequencies,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, HdocError> {
        let mut out = Vec::new();
        out.extend_from_slice(&SEARCH_MAGIC);
        out.extend_from_slice(&SEARCH_VERSION.to_le_bytes());
        out.extend_from_slice(&SEARCH_FLAG_COMPACT_TF.to_le_bytes());
        put_u32(&mut out, self.page_texts.len())?;
        put_u32(&mut out, self.postings.len())?;
        for text in &self.page_texts {
            put_u32(&mut out, text.len())?;
            out.extend_from_slice(text.as_bytes());
        }
        for (term, pages) in &self.postings {
            put_string(&mut out, term)?;
            let count = u32::try_from(pages.len()).map_err(|_| HdocError::LimitExceeded)?;
            varint::write_u32(count, &mut out);
            let frequencies = self.term_frequencies.get(term);
            let mut previous_page = 0u32;
            for (index, &page) in pages.iter().enumerate() {
                if index > 0 && page <= previous_page {
                    return Err(HdocError::InvalidFormat(
                        "search postings must be strictly increasing".into(),
                    ));
                }
                let delta = if index == 0 {
                    page
                } else {
                    page - previous_page
                };
                let tf = frequencies
                    .and_then(|items| items.get(index))
                    .filter(|(tf_page, _)| *tf_page == page)
                    .map(|(_, freq)| (*freq).max(1))
                    .unwrap_or(1);
                varint::write_u32(delta, &mut out);
                varint::write_u32(tf, &mut out);
                previous_page = page;
            }
        }
        Ok(out)
    }

    pub fn decode(input: &[u8]) -> Result<Self, HdocError> {
        let mut cursor = Cursor { input, position: 0 };
        if cursor.take(4)? != SEARCH_MAGIC {
            return Err(HdocError::InvalidFormat("missing HDSR search magic".into()));
        }
        let version = cursor.u16()?;
        if version != SEARCH_VERSION {
            return Err(HdocError::UnsupportedVersion(version));
        }
        let flags = cursor.u16()?;
        if flags & !SEARCH_FLAG_COMPACT_TF != 0 {
            return Err(HdocError::InvalidFormat(
                "unsupported search block flags".into(),
            ));
        }
        let compact_tf = flags & SEARCH_FLAG_COMPACT_TF != 0;
        let page_count = cursor.u32()? as usize;
        let term_count = cursor.u32()? as usize;
        validate_count(page_count, cursor.remaining(), 4)?;
        let mut page_texts = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            let length = cursor.u32()? as usize;
            let text = String::from_utf8(cursor.take(length)?.to_vec())
                .map_err(|_| HdocError::InvalidUtf8)?;
            page_texts.push(text);
        }
        let min_term_bytes = if compact_tf { 3 } else { 6 };
        validate_count(term_count, cursor.remaining(), min_term_bytes)?;
        let mut postings = BTreeMap::new();
        let mut term_frequencies = BTreeMap::new();
        for _ in 0..term_count {
            let term = cursor.string()?;
            if compact_tf {
                let count = cursor.varint_u32()? as usize;
                validate_count(count, cursor.remaining(), 2)?;
                let mut pages = Vec::with_capacity(count);
                let mut freqs = Vec::with_capacity(count);
                let mut current_page = 0u32;
                for index in 0..count {
                    let delta = cursor.varint_u32()?;
                    let tf = cursor.varint_u32()?;
                    if tf == 0 || (index > 0 && delta == 0) {
                        return Err(HdocError::InvalidFormat(
                            "invalid delta or zero frequency in search postings".into(),
                        ));
                    }
                    current_page = if index == 0 {
                        delta
                    } else {
                        current_page
                            .checked_add(delta)
                            .ok_or(HdocError::LimitExceeded)?
                    };
                    pages.push(current_page);
                    freqs.push((current_page, tf));
                }
                postings.insert(term.clone(), pages);
                term_frequencies.insert(term, freqs);
            } else {
                let count = cursor.u32()? as usize;
                validate_count(count, cursor.remaining(), 4)?;
                let mut pages = Vec::with_capacity(count);
                let mut freqs = Vec::with_capacity(count);
                for _ in 0..count {
                    let page = cursor.u32()?;
                    pages.push(page);
                    freqs.push((page, 1));
                }
                postings.insert(term.clone(), pages);
                term_frequencies.insert(term, freqs);
            }
        }
        if cursor.remaining() != 0 {
            return Err(HdocError::InvalidFormat(
                "unexpected data after search index".into(),
            ));
        }
        Ok(Self {
            page_texts,
            postings,
            term_frequencies,
        })
    }

    pub fn page_text(&self, page_id: u32) -> Option<&str> {
        self.page_texts.get(page_id as usize).map(String::as_str)
    }

    /// Extract a short UTF-8 context snippet around the first normalized match on `page_id`.
    pub fn snippet_for_page(
        &self,
        page_id: u32,
        query_terms: &[String],
        max_chars: usize,
    ) -> String {
        let Some(text) = self.page_text(page_id) else {
            return String::new();
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let chars: Vec<char> = trimmed.chars().collect();
        if chars.len() <= max_chars {
            return collapse_whitespace(trimmed);
        }
        let match_char_pos = find_first_matching_word_char_index(trimmed, query_terms).unwrap_or(0);
        let half = max_chars / 2;
        let start = match_char_pos.saturating_sub(half);
        let end = (start + max_chars).min(chars.len());
        let start = end.saturating_sub(max_chars);
        let slice: String = chars[start..end].iter().collect();
        let mut snippet = collapse_whitespace(&slice);
        if start > 0 {
            snippet.insert_str(0, "...");
        }
        if end < chars.len() {
            snippet.push_str("...");
        }
        snippet
    }
}

/// Tokenize text consistently for encoder indexing and decoder queries,
/// including Arabic Presentation Forms (NFKC), Arabic orthography, numerals,
/// and common Latin PDF ligatures.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in text.chars() {
        if is_ignored_mark(character) {
            continue;
        }
        if let Some(ligature) = expand_multi_char_ligature(character) {
            for sub in ligature.chars() {
                push_normalized_alphanumeric(sub, &mut token);
            }
            continue;
        }
        let mapped = map_presentation_form(character);
        if mapped.is_alphanumeric() {
            push_normalized_alphanumeric(mapped, &mut token);
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

pub fn build_index<'a>(pages: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, Vec<u32>> {
    build_index_with_frequencies(pages).0
}

#[allow(clippy::type_complexity)]
pub fn build_index_with_frequencies<'a>(
    pages: impl IntoIterator<Item = &'a str>,
) -> (
    BTreeMap<String, Vec<u32>>,
    BTreeMap<String, Vec<(u32, u32)>>,
) {
    let mut postings: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let mut frequencies: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    for (page_id, text) in pages.into_iter().enumerate() {
        if let Ok(page_id) = u32::try_from(page_id) {
            let mut page_counts: BTreeMap<String, u32> = BTreeMap::new();
            for term in tokenize(text) {
                let entry = page_counts.entry(term).or_insert(0);
                *entry = entry.saturating_add(1);
            }
            for (term, count) in page_counts {
                postings.entry(term.clone()).or_default().push(page_id);
                frequencies.entry(term).or_default().push((page_id, count));
            }
        }
    }
    (postings, frequencies)
}

fn push_normalized_alphanumeric(character: char, token: &mut String) {
    let normalized = match character {
        'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
        'ؤ' => 'و',
        'ئ' | 'ى' | 'ی' => 'ي',
        'ة' => 'ه',
        'ک' => 'ك',
        '\u{0660}'..='\u{0669}' => {
            char::from_u32(u32::from('0') + (u32::from(character) - 0x0660)).unwrap_or(character)
        }
        '\u{06F0}'..='\u{06F9}' => {
            char::from_u32(u32::from('0') + (u32::from(character) - 0x06F0)).unwrap_or(character)
        }
        other => other,
    };
    token.extend(normalized.to_lowercase());
}

fn expand_multi_char_ligature(character: char) -> Option<&'static str> {
    match character {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        '\u{FDF2}' => Some("الله"),
        '\u{FEF5}'..='\u{FEFC}' => Some("لا"),
        _ => None,
    }
}

fn map_presentation_form(character: char) -> char {
    match character as u32 {
        0xFB56..=0xFB59 => 'پ',
        0xFB7A..=0xFB7D => 'چ',
        0xFB8A..=0xFB8B => 'ژ',
        0xFB8E..=0xFB91 => 'ك',
        0xFB92..=0xFB95 => 'گ',
        0xFBFC..=0xFBFF => 'ي',
        0xFE80 => 'ء',
        0xFE81..=0xFE84 | 0xFE87..=0xFE88 | 0xFE8D..=0xFE8E => 'ا',
        0xFE85..=0xFE86 | 0xFEED..=0xFEEE => 'و',
        0xFE89..=0xFE8C | 0xFEEF..=0xFEF4 => 'ي',
        0xFE8F..=0xFE92 => 'ب',
        0xFE93..=0xFE94 | 0xFEE9..=0xFEEC => 'ه',
        0xFE95..=0xFE98 => 'ت',
        0xFE99..=0xFE9C => 'ث',
        0xFE9D..=0xFEA0 => 'ج',
        0xFEA1..=0xFEA4 => 'ح',
        0xFEA5..=0xFEA8 => 'خ',
        0xFEA9..=0xFEAA => 'د',
        0xFEAB..=0xFEAC => 'ذ',
        0xFEAD..=0xFEAE => 'ر',
        0xFEAF..=0xFEB0 => 'ز',
        0xFEB1..=0xFEB4 => 'س',
        0xFEB5..=0xFEB8 => 'ش',
        0xFEB9..=0xFEBC => 'ص',
        0xFEBD..=0xFEC0 => 'ض',
        0xFEC1..=0xFEC4 => 'ط',
        0xFEC5..=0xFEC8 => 'ظ',
        0xFEC9..=0xFECC => 'ع',
        0xFECD..=0xFED0 => 'غ',
        0xFED1..=0xFED4 => 'ف',
        0xFED5..=0xFED8 => 'ق',
        0xFED9..=0xFEDC => 'ك',
        0xFEDD..=0xFEE0 => 'ل',
        0xFEE1..=0xFEE4 => 'م',
        0xFEE5..=0xFEE8 => 'ن',
        _ => character,
    }
}

fn is_ignored_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0640 | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED | 0xFE70..=0xFE7F
    )
}

fn find_first_matching_word_char_index(text: &str, query_terms: &[String]) -> Option<usize> {
    let mut word_start_char = None;
    for (char_index, (byte_index, ch)) in text.char_indices().enumerate() {
        if is_ignored_mark(ch) {
            continue;
        }
        let is_word_char =
            expand_multi_char_ligature(ch).is_some() || map_presentation_form(ch).is_alphanumeric();
        if is_word_char {
            if word_start_char.is_none() {
                word_start_char = Some((char_index, byte_index));
            }
        } else if let Some((start_char, start_byte)) = word_start_char.take() {
            let candidate = &text[start_byte..byte_index];
            if tokenize(candidate).iter().any(|tok| {
                query_terms
                    .iter()
                    .any(|q| tok == q || tok.starts_with(q.as_str()))
            }) {
                return Some(start_char);
            }
        }
    }
    if let Some((start_char, start_byte)) = word_start_char {
        let candidate = &text[start_byte..];
        if tokenize(candidate).iter().any(|tok| {
            query_terms
                .iter()
                .any(|q| tok == q || tok.starts_with(q.as_str()))
        }) {
            return Some(start_char);
        }
    }
    None
}

fn collapse_whitespace(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space && !output.is_empty() {
                output.push(' ');
                prev_space = true;
            }
        } else {
            output.push(ch);
            prev_space = false;
        }
    }
    if output.ends_with(' ') {
        output.pop();
    }
    output
}

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<(), HdocError> {
    let value = u32::try_from(value).map_err(|_| HdocError::LimitExceeded)?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_string(output: &mut Vec<u8>, value: &str) -> Result<(), HdocError> {
    let length = u16::try_from(value.len()).map_err(|_| HdocError::LimitExceeded)?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn validate_count(count: usize, remaining: usize, min_size: usize) -> Result<(), HdocError> {
    if count
        .checked_mul(min_size)
        .ok_or(HdocError::LimitExceeded)?
        > remaining
    {
        return Err(HdocError::Truncated);
    }
    Ok(())
}

struct Cursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.position)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], HdocError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(HdocError::LimitExceeded)?;
        let value = self
            .input
            .get(self.position..end)
            .ok_or(HdocError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, HdocError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("fixed integer slice"),
        ))
    }

    fn u32(&mut self) -> Result<u32, HdocError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("fixed integer slice"),
        ))
    }

    fn varint_u32(&mut self) -> Result<u32, HdocError> {
        varint::read_u32(self.input, &mut self.position)
    }

    fn string(&mut self) -> Result<String, HdocError> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| HdocError::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::{tokenize, SearchBlock};

    #[test]
    fn search_text_keeps_unicode_and_normalizes_arabic_diacritics() {
        assert_eq!(tokenize("السَّلَامُ عَلَيْكُمْ"), vec!["السلام", "عليكم"]);
        let original = SearchBlock::from_page_texts(vec!["مرحبا بالعالم".into()]);
        let encoded = original.encode().unwrap();
        assert_eq!(SearchBlock::decode(&encoded).unwrap(), original);
    }

    #[test]
    fn normalizes_arabic_presentation_forms_ligatures_and_digits() {
        assert_eq!(
            tokenize("ﻻ ﺇﻟﻪ ﺇﻻ \u{FDF2} مدرسة ١٢٣ \u{FB01}le"),
            tokenize("لا اله الا الله مدرسه 123 file")
        );
    }

    #[test]
    fn decodes_legacy_uncompressed_postings_format() {
        let mut legacy = Vec::new();
        legacy.extend_from_slice(b"HDSR");
        legacy.extend_from_slice(&0x0100u16.to_le_bytes());
        legacy.extend_from_slice(&0u16.to_le_bytes());
        legacy.extend_from_slice(&2u32.to_le_bytes());
        legacy.extend_from_slice(&1u32.to_le_bytes());
        for text in ["hello", "hello world"] {
            legacy.extend_from_slice(&(text.len() as u32).to_le_bytes());
            legacy.extend_from_slice(text.as_bytes());
        }
        legacy.extend_from_slice(&5u16.to_le_bytes());
        legacy.extend_from_slice(b"hello");
        legacy.extend_from_slice(&2u32.to_le_bytes());
        legacy.extend_from_slice(&0u32.to_le_bytes());
        legacy.extend_from_slice(&1u32.to_le_bytes());

        let decoded = SearchBlock::decode(&legacy).unwrap();
        assert_eq!(decoded.postings.get("hello"), Some(&vec![0, 1]));
        assert_eq!(
            decoded.term_frequencies.get("hello"),
            Some(&vec![(0, 1), (1, 1)])
        );
    }
}
