use crate::Result;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

pub fn normalize(text: &str) -> String {
    text.nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfc()
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    pub normalized: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut end = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if grapheme.chars().next().is_some_and(char::is_alphabetic) {
            start.get_or_insert(offset);
            end = offset + grapheme.len();
        } else if let Some(s) = start.take() {
            tokens.push(Token {
                normalized: normalize(&text[s..end]),
                start_byte: s,
                end_byte: end,
            });
        }
    }
    if let Some(s) = start {
        tokens.push(Token {
            normalized: normalize(&text[s..end]),
            start_byte: s,
            end_byte: end,
        });
    }
    tokens
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TailEdit {
    pub start_byte: usize,
    pub replacement: String,
}

impl TailEdit {
    pub fn apply(&self, q: &str) -> Result<String> {
        let prefix = q.get(..self.start_byte).ok_or("invalid edit boundary")?;
        if self.start_byte != q.len()
            && !q.grapheme_indices(true).any(|(i, _)| i == self.start_byte)
        {
            return Err("edit splits grapheme".into());
        }
        Ok(format!("{prefix}{}", self.replacement))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub text: Vec<String>,
    pub pos: i32,
    #[serde(rename = "endOfWord")]
    pub end_of_word: bool,
}

pub fn project(q: &str, edits: &[TailEdit]) -> Result<Response> {
    let start = edits.iter().map(|e| e.start_byte).min().unwrap_or(q.len());
    let mut text = Vec::new();
    for edit in edits {
        edit.apply(q)?;
        text.push(format!(
            "{}{}",
            q.get(start..edit.start_byte).ok_or("invalid span")?,
            edit.replacement
        ));
    }
    Ok(Response {
        end_of_word: !text.is_empty(),
        text,
        pos: -i32::try_from(q[start..].encode_utf16().count())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_original_spans_and_independent_utf16_application() {
        let q = "😀 Е\u{308}ж при вет";
        let ts = tokenize(q);
        assert_eq!(ts[0].normalized, "ёж");
        assert_eq!(&q[ts[0].start_byte..ts[0].end_byte], "Е\u{308}ж");
        let edits = vec![
            TailEdit {
                start_byte: ts[1].start_byte,
                replacement: "привет".into(),
            },
            TailEdit {
                start_byte: ts[2].start_byte,
                replacement: "ветер".into(),
            },
        ];
        let response = project(q, &edits).unwrap();
        let units: Vec<u16> = q.encode_utf16().collect();
        for (edit, insertion) in edits.iter().zip(response.text) {
            let mut applied = units[..(units.len() as i32 + response.pos) as usize].to_vec();
            applied.extend(insertion.encode_utf16());
            assert_eq!(
                String::from_utf16(&applied).unwrap(),
                edit.apply(q).unwrap()
            );
        }
        assert!(TailEdit {
            start_byte: 7,
            replacement: String::new()
        }
        .apply(q)
        .is_err());
    }
}
