use std::collections::{BTreeMap, BTreeSet};

use crate::HdocError;

const SEARCH_MAGIC: [u8; 4] = *b"HDSR";
const SEARCH_VERSION: u16 = 0x0100;

/// Search data is stored separately from drawing opcodes so shaped text can
/// use glyph IDs without losing its original UTF-8 representation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchBlock {
    pub page_texts: Vec<String>,
    pub postings: BTreeMap<String, Vec<u32>>,
}

impl SearchBlock {
    pub fn from_page_texts(page_texts: Vec<String>) -> Self {
        let postings = build_index(page_texts.iter().map(String::as_str));
        Self {
            page_texts,
            postings,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, HdocError> {
        let mut out = Vec::new();
        out.extend_from_slice(&SEARCH_MAGIC);
        out.extend_from_slice(&SEARCH_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        put_u32(&mut out, self.page_texts.len())?;
        put_u32(&mut out, self.postings.len())?;
        for text in &self.page_texts {
            put_u32(&mut out, text.len())?;
            out.extend_from_slice(text.as_bytes());
        }
        for (term, pages) in &self.postings {
            put_string(&mut out, term)?;
            put_u32(&mut out, pages.len())?;
            for page in pages {
                out.extend_from_slice(&page.to_le_bytes());
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
        let _flags = cursor.u16()?;
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
        validate_count(term_count, cursor.remaining(), 6)?;
        let mut postings = BTreeMap::new();
        for _ in 0..term_count {
            let term = cursor.string()?;
            let count = cursor.u32()? as usize;
            validate_count(count, cursor.remaining(), 4)?;
            let mut pages = Vec::with_capacity(count);
            for _ in 0..count {
                pages.push(cursor.u32()?);
            }
            postings.insert(term, pages);
        }
        if cursor.remaining() != 0 {
            return Err(HdocError::InvalidFormat(
                "unexpected data after search index".into(),
            ));
        }
        Ok(Self {
            page_texts,
            postings,
        })
    }

    pub fn page_text(&self, page_id: u32) -> Option<&str> {
        self.page_texts.get(page_id as usize).map(String::as_str)
    }
}

/// Tokenize text consistently for encoder indexing and decoder queries.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in text.chars() {
        if is_arabic_mark(character) || character == '\u{0640}' {
            continue;
        }
        if character.is_alphanumeric() {
            let normalized = match character {
                'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
                'ى' => 'ي',
                other => other,
            };
            token.extend(normalized.to_lowercase());
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
    let mut result: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (page_id, text) in pages.into_iter().enumerate() {
        if let Ok(page_id) = u32::try_from(page_id) {
            let unique: BTreeSet<_> = tokenize(text).into_iter().collect();
            for term in unique {
                result.entry(term).or_default().push(page_id);
            }
        }
    }
    result
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
    fn string(&mut self) -> Result<String, HdocError> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| HdocError::InvalidUtf8)
    }
}

fn is_arabic_mark(character: char) -> bool {
    matches!(character as u32, 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED)
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
}
