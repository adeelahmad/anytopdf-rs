//! CLIP's byte-level BPE tokenizer.
//!
//! Reads either OpenAI's `bpe_simple_vocab_16e6.txt(.gz)` merges list or a
//! Hugging Face `tokenizer.json`, and reproduces OpenAI CLIP's
//! `SimpleTokenizer` (lowercase, collapsed whitespace, `</w>` word ends).

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;
use std::{collections::HashMap, fs, io::Read, path::Path};

pub const CONTEXT_LENGTH: usize = 77;
const START: &str = "<|startoftext|>";
const END: &str = "<|endoftext|>";
/// Merges OpenAI CLIP uses from its vocabulary file (49152 - 256 - 2).
const OPENAI_MERGES: usize = 48894;

pub struct Tokenizer {
    encoder: HashMap<String, i64>,
    ranks: HashMap<(String, String), usize>,
    byte_encoder: [char; 256],
    pattern: Regex,
    start: i64,
    end: i64,
}

/// GPT-2's reversible mapping from bytes to printable unicode characters.
fn bytes_to_unicode() -> [char; 256] {
    let printable = |b: u32| (33..=126).contains(&b) || (161..=172).contains(&b) || b >= 174;
    let mut table = ['\0'; 256];
    let mut extra = 0;
    for b in 0..256u32 {
        table[b as usize] = if printable(b) {
            char::from_u32(b).unwrap_or('\0')
        } else {
            extra += 1;
            char::from_u32(255 + extra).unwrap_or('\0')
        };
    }
    table
}

/// Bytes in the order OpenAI lists them as base vocabulary entries.
fn base_order() -> Vec<u32> {
    let printable = |b: u32| (33..=126).contains(&b) || (161..=172).contains(&b) || b >= 174;
    let mut order: Vec<u32> = (0..256).filter(|&b| printable(b)).collect();
    order.extend((0..256).filter(|&b| !printable(b)));
    order
}

impl Tokenizer {
    /// Loads `tokenizer.json`, or an OpenAI merges file (`.txt` or `.txt.gz`).
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let text = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut out = String::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .read_to_string(&mut out)
                .with_context(|| format!("decompress {}", path.display()))?;
            out
        } else {
            String::from_utf8(bytes).context("tokenizer file is not UTF-8")?
        };
        if text.trim_start().starts_with('{') {
            Self::from_tokenizer_json(&text)
        } else {
            Self::from_openai_merges(&text)
        }
    }

    pub fn from_openai_merges(text: &str) -> Result<Self> {
        let merges: Vec<(String, String)> = text
            .lines()
            .skip(1)
            .take(OPENAI_MERGES)
            .filter_map(|line| {
                let (a, b) = line.split_once(' ')?;
                Some((a.to_string(), b.to_string()))
            })
            .collect();
        if merges.is_empty() {
            bail!("tokenizer merges file has no merges");
        }
        let byte_encoder = bytes_to_unicode();
        let mut vocab: Vec<String> = base_order()
            .iter()
            .map(|&b| byte_encoder[b as usize].to_string())
            .collect();
        let words: Vec<String> = vocab.iter().map(|v| format!("{v}</w>")).collect();
        vocab.extend(words);
        vocab.extend(merges.iter().map(|(a, b)| format!("{a}{b}")));
        vocab.push(START.into());
        vocab.push(END.into());
        let encoder = vocab
            .into_iter()
            .enumerate()
            .map(|(i, token)| (token, i as i64))
            .collect();
        Self::build(encoder, merges)
    }

    pub fn from_tokenizer_json(text: &str) -> Result<Self> {
        let json: Value = serde_json::from_str(text).context("parse tokenizer.json")?;
        let model = &json["model"];
        let mut encoder: HashMap<String, i64> = model["vocab"]
            .as_object()
            .context("tokenizer.json has no model.vocab")?
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
            .collect();
        for added in json["added_tokens"].as_array().into_iter().flatten() {
            if let (Some(content), Some(id)) = (added["content"].as_str(), added["id"].as_i64()) {
                encoder.insert(content.to_string(), id);
            }
        }
        let merges = model["merges"]
            .as_array()
            .context("tokenizer.json has no model.merges")?
            .iter()
            .filter_map(|m| match m {
                Value::String(s) => s
                    .split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string())),
                Value::Array(pair) => Some((
                    pair.first()?.as_str()?.to_string(),
                    pair.get(1)?.as_str()?.to_string(),
                )),
                _ => None,
            })
            .collect();
        Self::build(encoder, merges)
    }

    fn build(encoder: HashMap<String, i64>, merges: Vec<(String, String)>) -> Result<Self> {
        let start = *encoder
            .get(START)
            .context("vocabulary has no <|startoftext|>")?;
        let end = *encoder
            .get(END)
            .context("vocabulary has no <|endoftext|>")?;
        let ranks = merges
            .into_iter()
            .enumerate()
            .map(|(rank, pair)| (pair, rank))
            .collect();
        let pattern = Regex::new(
            r"<\|startoftext\|>|<\|endoftext\|>|'s|'t|'re|'ve|'m|'ll|'d|\p{L}+|\p{N}|[^\s\p{L}\p{N}]+",
        )?;
        Ok(Self {
            encoder,
            ranks,
            byte_encoder: bytes_to_unicode(),
            pattern,
            start,
            end,
        })
    }

    /// Token ids for `text` without the start and end markers.
    pub fn encode(&self, text: &str) -> Vec<i64> {
        let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let lowered = cleaned.to_lowercase();
        let mut ids = Vec::new();
        for token in self.pattern.find_iter(&lowered) {
            let unicode: String = token
                .as_str()
                .bytes()
                .map(|b| self.byte_encoder[b as usize])
                .collect();
            for piece in self.bpe(&unicode) {
                if let Some(&id) = self.encoder.get(&piece) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Fixed-length model input: start marker, tokens (truncated), end marker,
    /// then zero padding; plus the matching attention mask.
    pub fn encode_padded(&self, text: &str) -> (Vec<i64>, Vec<i64>) {
        let mut ids = vec![self.start];
        ids.extend(self.encode(text).into_iter().take(CONTEXT_LENGTH - 2));
        ids.push(self.end);
        let mut mask = vec![1; ids.len()];
        ids.resize(CONTEXT_LENGTH, 0);
        mask.resize(CONTEXT_LENGTH, 0);
        (ids, mask)
    }

    fn bpe(&self, token: &str) -> Vec<String> {
        let mut word: Vec<String> = token.chars().map(String::from).collect();
        match word.last_mut() {
            Some(last) => last.push_str("</w>"),
            None => return Vec::new(),
        }
        while word.len() > 1 {
            let best = word
                .windows(2)
                .enumerate()
                .filter_map(|(i, pair)| {
                    self.ranks
                        .get(&(pair[0].clone(), pair[1].clone()))
                        .map(|&rank| (rank, i))
                })
                .min();
            let Some((_, i)) = best else { break };
            let (first, second) = (word[i].clone(), word[i + 1].clone());
            let mut merged = Vec::with_capacity(word.len());
            let mut i = 0;
            while i < word.len() {
                if i + 1 < word.len() && word[i] == first && word[i + 1] == second {
                    merged.push(format!("{first}{second}"));
                    i += 2;
                } else {
                    merged.push(word[i].clone());
                    i += 1;
                }
            }
            word = merged;
        }
        word
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny merges file in OpenAI's format: header line, then merges.
    fn tiny() -> Tokenizer {
        Tokenizer::from_openai_merges("#version: 0.2\nr e\nre d</w>\nc a\nca t</w>\n").unwrap()
    }

    #[test]
    fn merges_words_and_marks_word_ends() {
        let t = tiny();
        let ids = t.encode("Red  CAT");
        let red = t.encoder["red</w>"];
        let cat = t.encoder["cat</w>"];
        assert_eq!(ids, vec![red, cat]);
    }

    #[test]
    fn unmerged_text_falls_back_to_byte_tokens() {
        let t = tiny();
        let ids = t.encode("ox!");
        assert_eq!(
            ids,
            vec![t.encoder["o"], t.encoder["x</w>"], t.encoder["!</w>"]]
        );
    }

    #[test]
    fn padded_input_has_markers_mask_and_fixed_length() {
        let t = tiny();
        let (ids, mask) = t.encode_padded("red");
        assert_eq!(ids.len(), CONTEXT_LENGTH);
        assert_eq!(&ids[..3], &[t.start, t.encoder["red</w>"], t.end]);
        assert!(ids[3..].iter().all(|&id| id == 0));
        assert_eq!(mask.iter().sum::<i64>(), 3);
        // OpenAI's vocabulary layout: 256 bytes, 256 word-end bytes, merges, markers.
        assert_eq!(t.start, 512 + 4);
        assert_eq!(t.end, 512 + 5);
    }

    #[test]
    fn long_text_is_truncated_but_keeps_the_end_marker() {
        let t = tiny();
        let (ids, mask) = t.encode_padded(&"red ".repeat(200));
        assert_eq!(ids[CONTEXT_LENGTH - 1], t.end);
        assert_eq!(mask.iter().sum::<i64>(), CONTEXT_LENGTH as i64);
    }

    #[test]
    fn reads_hugging_face_tokenizer_json() {
        let json = serde_json::json!({
            "added_tokens": [
                {"id": 10, "content": "<|startoftext|>"},
                {"id": 11, "content": "<|endoftext|>"}
            ],
            "model": {
                "vocab": {"r": 0, "e": 1, "d</w>": 2, "re": 3, "red</w>": 4},
                "merges": ["r e", ["re", "d</w>"]]
            }
        });
        let t = Tokenizer::from_tokenizer_json(&json.to_string()).unwrap();
        assert_eq!(t.encode("RED"), vec![4]);
        assert_eq!(t.encode_padded("red").0[..3], [10, 4, 11]);
    }
}
