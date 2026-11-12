//! Language-aware analysis for the embedded full-text index (RFC-007 7.2).
//!
//! The tantivy built-in `en_stem` chain treats a run of CJK characters as ONE
//! token (CJK has no spaces) and then drops any token longer than 40 *bytes*.
//! CJK characters are 3 bytes in UTF-8, so every Chinese/Japanese/Korean run
//! longer than 13 characters is silently removed from the index — the
//! document becomes unsearchable. This module defines the SearchOS analyzer:
//!
//! - **Latin/digit runs** pass through unchanged and are lowercased and
//!   English-stemmed downstream (same terms the old chain produced).
//! - **CJK runs** (Han, Kana, Hangul) are split into overlapping character
//!   bigrams — the standard Lucene `CJKBigramFilter` semantics. A
//!   single-character run passes through as itself so single-character
//!   queries still match.
//!
//! The analyzer is registered under [`SEARCHOS_TOKENIZER`] for fresh indexes.
//! Indexes created before the analyzer existed store `en_stem` in their
//! schema; for those we ALSO register the analyzer under the legacy name so
//! writes, queries and a plain `rebuild_from` all re-tokenize with the new
//! analyzer without any directory migration (old CJK rows only become
//! searchable after a `searchos index rebuild`, same as before the change).

use std::collections::VecDeque;

use tantivy::tokenizer::Language;
use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer, Token, TokenFilter,
    TokenStream, Tokenizer,
};

/// Tokenizer name referenced by the schema of every new index.
pub const SEARCHOS_TOKENIZER: &str = "searchos_mixed";

/// Tokenizer name stored in indexes created before CJK analysis existed.
pub const LEGACY_TOKENIZER: &str = "en_stem";

/// Upper bound of terms extracted from one query (a long CJK sentence can
/// expand to many bigrams; the boolean query is bounded for latency).
pub const MAX_QUERY_TERMS: usize = 64;

/// Registers the SearchOS analyzer on an index's tokenizer manager.
///
/// Called on every index open/create. The legacy-name override keeps old
/// (pre-CJK) indexes on the same code path: their stored schema still says
/// `en_stem`, and with the override a plain `rebuild_from` re-tokenizes all
/// rows with the new analyzer in place.
pub fn register_analyzers(index: &tantivy::Index) {
    let analyzer = build_analyzer();
    index
        .tokenizers()
        .register(SEARCHOS_TOKENIZER, analyzer.clone());
    index.tokenizers().register(LEGACY_TOKENIZER, analyzer);
}

/// Whether the index's stored schema uses the current analyzer. `false` for
/// indexes created before CJK analysis existed (diagnostics / doctor output;
/// the analyzer override above keeps such indexes working).
pub fn schema_uses_current_analyzer(
    schema: &tantivy::schema::Schema,
    body_field: tantivy::schema::Field,
) -> bool {
    let entry = schema.get_field_entry(body_field);
    match entry.field_type() {
        tantivy::schema::FieldType::Str(options) => {
            options
                .get_indexing_options()
                .map(|o| o.tokenizer().to_string())
                .unwrap_or_default()
                == SEARCHOS_TOKENIZER
        }
        _ => false,
    }
}

/// Builds the SearchOS text analyzer.
///
/// Chain: `SimpleTokenizer` (one token per alphanumeric run, CJK included)
/// → [`CjkBigramFilter`] (split CJK runs into bigrams)
/// → `LowerCaser` → `RemoveLongFilter(40)` (Latin guard, CJK bigrams are 6
/// bytes) → English `Stemmer` (CJK tokens pass through unchanged).
pub fn build_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(CjkBigramFilter)
        .filter(LowerCaser)
        .filter(RemoveLongFilter::limit(40))
        .filter(Stemmer::new(Language::English))
        .build()
}

/// Unicode script ranges treated as CJK (Han, Kana, Hangul).
fn is_cjk(c: char) -> bool {
    let c = c as u32;
    (0x3040..=0x30FF).contains(&c)      // Hiragana + Katakana
        || (0x3400..=0x4DBF).contains(&c) // CJK Extension A
        || (0x4E00..=0x9FFF).contains(&c) // CJK Unified Ideographs
        || (0xF900..=0xFAFF).contains(&c) // CJK Compatibility Ideographs
        || (0x1100..=0x11FF).contains(&c) // Hangul Jamo
        || (0xAC00..=0xD7AF).contains(&c) // Hangul Syllables
        || (0x20000..=0x2FA1F).contains(&c) // CJK Extensions B..F
}

/// A token filter that splits alphanumeric runs into CJK bigrams.
#[derive(Clone, Default)]
pub struct CjkBigramFilter;

impl TokenFilter for CjkBigramFilter {
    type Tokenizer<T: Tokenizer> = CjkBigramTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        CjkBigramTokenizer { inner: tokenizer }
    }
}

#[derive(Clone)]
pub struct CjkBigramTokenizer<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for CjkBigramTokenizer<T> {
    type TokenStream<'a> = CjkBigramStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CjkBigramStream {
            tail: self.inner.token_stream(text),
            pending: VecDeque::new(),
            current: Token::default(),
            next_position: 0,
        }
    }
}

/// Emits several tokens per input token: CJK runs become overlapping
/// bigrams (adjacent positions), non-CJK runs pass through as one token.
pub struct CjkBigramStream<T: TokenStream> {
    tail: T,
    pending: VecDeque<Token>,
    current: Token,
    next_position: usize,
}

impl<T: TokenStream> TokenStream for CjkBigramStream<T> {
    fn advance(&mut self) -> bool {
        loop {
            if let Some(mut token) = self.pending.pop_front() {
                // Sequential positions keep phrase adjacency meaningful for
                // bigrams; `position_length` stays 1.
                token.position = self.next_position;
                self.next_position += 1;
                self.current = token;
                return true;
            }
            if !self.tail.advance() {
                return false;
            }
            let base = self.tail.token().clone();
            self.pending.extend(split_runs(&base));
            // A token can expand to nothing only if it was empty; keep
            // draining the tail either way.
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

/// Splits one base token into CJK bigram tokens and plain runs.
fn split_runs(base: &Token) -> Vec<Token> {
    let text = &base.text;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.iter().all(|(_, c)| !is_cjk(*c)) {
        // Fast path: no CJK — keep the token exactly as the old chain did.
        return vec![base.clone()];
    }
    // Byte offset of char `idx` within the base token; `idx == len` means
    // one-past-the-end (the token's full byte length).
    let byte_at = |idx: usize| -> usize {
        if idx >= chars.len() {
            text.len()
        } else {
            chars[idx].0
        }
    };
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let cjk = is_cjk(chars[start].1);
        let mut end = start;
        while end < chars.len() && is_cjk(chars[end].1) == cjk {
            end += 1;
        }
        let offset_from = base.offset_from + byte_at(start);
        let offset_to = base.offset_from + byte_at(end);
        if !cjk {
            out.push(Token {
                offset_from,
                offset_to,
                position: 0,
                text: text[chars[start].0..chars[end].0].to_string(),
                position_length: 1,
            });
        } else {
            let run: Vec<char> = chars[start..end].iter().map(|(_, c)| *c).collect();
            if run.len() == 1 {
                out.push(Token {
                    offset_from,
                    offset_to,
                    position: 0,
                    text: run[0].to_string(),
                    position_length: 1,
                });
            } else {
                for w in 0..run.len() - 1 {
                    out.push(Token {
                        offset_from: base.offset_from + byte_at(start + w),
                        offset_to: base.offset_from + byte_at(start + w + 2),
                        position: 0,
                        text: run[w..w + 2].iter().collect(),
                        position_length: 1,
                    });
                }
            }
        }
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::tokenizer::TextAnalyzer;

    fn tokens(analyzer: &mut TextAnalyzer, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stream = analyzer.token_stream(text);
        stream.process(&mut |t| out.push(t.text.clone()));
        out
    }

    #[test]
    fn cjk_sentence_expands_to_bigrams() {
        let mut a = build_analyzer();
        let toks = tokens(&mut a, "本地优先检索引擎");
        // 8 chars -> 7 overlapping bigrams, all present.
        assert_eq!(
            toks,
            vec!["本地", "地优", "优先", "先检", "检索", "索引", "引擎"]
        );
    }

    #[test]
    fn long_cjk_sentence_is_not_dropped() {
        // Regression: the old en_stem chain tokenized this to [] because the
        // 40-byte RemoveLongFilter dropped every 3-byte-char run.
        let mut a = build_analyzer();
        let toks = tokens(
            &mut a,
            "这是关于本地优先检索引擎的详细说明。系统会先在本地知识库解析问题。",
        );
        assert!(toks.contains(&"检索".to_string()));
        assert!(toks.contains(&"引擎".to_string()));
        assert!(toks.contains(&"解析".to_string()));
        assert_eq!(toks.len() % 2, 1, "sentence splits at 。 into two odd runs");
    }

    #[test]
    fn single_cjk_char_survives_for_single_char_queries() {
        let mut a = build_analyzer();
        assert_eq!(tokens(&mut a, "索"), vec!["索"]);
    }

    #[test]
    fn latin_tokens_unchanged_and_stemmed() {
        let mut a = build_analyzer();
        assert_eq!(
            tokens(&mut a, "Rust is an asynchronous runtime"),
            vec!["rust", "is", "an", "asynchron", "runtim"]
        );
    }

    #[test]
    fn mixed_latin_cjk_token_is_split() {
        let mut a = build_analyzer();
        let toks = tokens(&mut a, "Rust检索引擎");
        assert_eq!(toks, vec!["rust", "检索", "索引", "引擎"]);
    }

    #[test]
    fn offsets_point_into_the_original_text() {
        let mut a = build_analyzer();
        let text = "go 检索引擎";
        let mut stream = a.token_stream(text);
        let mut seen = Vec::new();
        while let Some(t) = stream.next() {
            seen.push((t.text.clone(), &text[t.offset_from..t.offset_to]));
        }
        // Every bigram's offsets must slice back to its own characters.
        for (token_text, slice) in &seen {
            assert_eq!(token_text, slice, "offset mismatch for {token_text}");
        }
        assert!(seen.iter().any(|(t, _)| t == "检索"));
    }

    #[test]
    fn positions_are_sequential_across_expansions() {
        let mut a = build_analyzer();
        let mut stream = a.token_stream("rust 检索引擎 go");
        let mut positions = Vec::new();
        while let Some(t) = stream.next() {
            positions.push(t.position);
        }
        // rust(0) + 3 bigrams(1..3) + go(4) = 5 strictly increasing tokens.
        assert_eq!(positions, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn japanese_and_korean_ranges_are_cjk() {
        let mut a = build_analyzer();
        assert!(tokens(&mut a, "検索エンジン").contains(&"検索".to_string()));
        assert!(tokens(&mut a, "검색엔진").contains(&"검색".to_string()));
    }

    #[test]
    fn registration_overrides_the_legacy_name() {
        let index = tantivy::Index::create_in_ram(build_schema_for_registration());
        register_analyzers(&index);
        let mut analyzer = index.tokenizers().get(LEGACY_TOKENIZER).unwrap();
        assert_eq!(
            tokens(&mut analyzer, "检索引擎"),
            vec!["检索", "索引", "引擎"]
        );
    }

    fn build_schema_for_registration() -> tantivy::schema::Schema {
        use tantivy::schema::{Schema, TEXT};
        let mut sb = Schema::builder();
        sb.add_text_field("body", TEXT);
        sb.build()
    }
}
