//! RFC-037: deterministic cross-lingual query variants.
//!
//! The analyzer can tokenize CJK and English, but a Chinese query still
//! cannot match an English document whose only shared concept is a term the
//! two languages spell differently. This module is a CURATED, tiny term
//! dictionary — not machine translation, not an LLM, no network call. Every
//! variant is reproducible and explainable from the term pairs that fired.
//!
//! Honesty limits (documented in the coverage matrix):
//! * it translates terms, not grammar or full sentences;
//! * the table is a starter set of domain terms, not a general dictionary;
//! * a variant is only an EXTRA recall list, fused by RRF — it never replaces
//!   the original query, so a query that already matches stays matched.

use crate::language::detect_language;

/// Bidirectional term pairs: `(zh, en)`.
///
/// Deliberately small and domain-relevant: SearchOS is search/Agent
/// infrastructure, so the table covers the vocabulary its own corpus and
/// users actually use. Adding a pair is a data change, not an algorithm
/// change — a curated resource can replace this table later.
pub const TERM_PAIRS: &[(&str, &str)] = &[
    ("检索", "retrieval"),
    ("搜索", "search"),
    ("搜索引擎", "search engine"),
    ("排序", "ranking"),
    ("排序算法", "ranking algorithm"),
    ("分词", "tokenization"),
    ("中文分词", "chinese tokenization"),
    ("向量", "vector"),
    ("向量检索", "vector search"),
    ("嵌入", "embedding"),
    ("缓存", "cache"),
    ("语义缓存", "semantic cache"),
    ("索引", "index"),
    ("全文索引", "full text index"),
    ("相关性", "relevance"),
    ("融合", "fusion"),
    ("异步", "async"),
    ("并发", "concurrency"),
    ("配置", "configuration"),
    ("数据库", "database"),
    ("性能", "performance"),
    ("监控", "monitoring"),
    ("机器学习", "machine learning"),
    ("框架", "framework"),
    ("部署", "deployment"),
    ("版本", "version"),
    ("日志", "logging"),
    ("重试", "retry"),
    ("限流", "rate limiting"),
    ("文档", "documentation"),
    ("证据", "evidence"),
    ("质量", "quality"),
    ("新鲜度", "freshness"),
    ("扩展", "expansion"),
    ("查询扩展", "query expansion"),
];

/// Longest CJK terms first so "排序算法" wins over "排序" on the same query.
fn terms_for_language(language: &str) -> Vec<(&'static str, &'static str)> {
    let mut pairs: Vec<(&str, &str)> = if language == "zh" {
        TERM_PAIRS.iter().map(|(zh, en)| (*zh, *en)).collect()
    } else {
        TERM_PAIRS.iter().map(|(zh, en)| (*en, *zh)).collect()
    };
    pairs.sort_by(|a, b| {
        b.0.chars()
            .count()
            .cmp(&a.0.chars().count())
            .then_with(|| a.0.cmp(b.0))
    });
    pairs
}

/// Replace whole TOKEN occurrences of `needle` with `replacement`.
///
/// CJK terms have no whitespace boundaries, so they are matched as
/// substrings; Latin terms require token boundaries so `index` never
/// rewritten inside `indexing` unless the whole term is present. The
/// replacement is a term, never a sentence, so grammar is out of scope.
fn replace_term(query: &str, needle: &str, replacement: &str) -> Option<String> {
    if needle.is_empty() || replacement.is_empty() {
        return None;
    }
    if needle
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        // Latin phrase / word: token-boundary match, case-insensitive.
        let lower = query.to_lowercase();
        let needle_lower = needle.to_lowercase();
        let mut search_from = 0usize;
        while let Some(offset) = lower[search_from..].find(&needle_lower) {
            let start = search_from + offset;
            let end = start + needle_lower.len();
            let before_ok = start == 0
                || !query[..start]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_alphanumeric());
            let after_ok = end == query.len()
                || !query[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric());
            if before_ok && after_ok {
                let mut out = String::with_capacity(query.len() + replacement.len());
                out.push_str(&query[..start]);
                out.push_str(replacement);
                out.push_str(&query[end..]);
                return Some(out);
            }
            search_from = end;
        }
        None
    } else {
        // CJK term: substring match (no word boundaries in the script).
        query.find(needle).map(|start| {
            let end = start + needle.len();
            let mut out = String::with_capacity(query.len() + replacement.len());
            out.push_str(&query[..start]);
            out.push_str(replacement);
            out.push_str(&query[end..]);
            out
        })
    }
}

/// Cross-language variants for a query.
///
/// Each variant swaps ONE known term into the other language (so a reviewer
/// can see which term fired). The original query is NEVER included — a
/// variant is an extra recall list, not a replacement. `max` bounds work,
/// and duplicates are removed deterministically.
pub fn cross_lingual_variants(query: &str, max: usize) -> Vec<String> {
    if max == 0 || query.trim().is_empty() {
        return Vec::new();
    }
    let language = detect_language(query);
    if language != "zh" && language != "en" {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let lower = query.to_lowercase();
    let mut fired_terms: Vec<String> = Vec::new();
    for (from, to) in terms_for_language(&language) {
        // A shorter term inside an already-translated longer term (排序 in
        // 排序算法) would produce an overlapping, misleading variant.
        if fired_terms
            .iter()
            .any(|fired| fired.to_lowercase().contains(&from.to_lowercase()))
        {
            continue;
        }
        if !lower.contains(&from.to_lowercase()) && !query.contains(from) {
            continue;
        }
        // Skip when the ORIGINAL query already contains the target-language
        // term: translating it would produce no new recall.
        if lower.contains(&to.to_lowercase()) || query.contains(to) {
            continue;
        }
        if let Some(variant) = replace_term(query, from, to) {
            if variant != query && !out.contains(&variant) {
                out.push(variant);
                fired_terms.push(from.to_string());
            }
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// All terms of the query translated in one shot. Used for a second, wider
/// recall pass when a single-term variant is not enough.
pub fn full_cross_lingual_variant(query: &str) -> Option<String> {
    let language = detect_language(query);
    if language != "zh" && language != "en" {
        return None;
    }
    let mut out = query.to_string();
    let mut changed = false;
    for (from, to) in terms_for_language(&language) {
        if let Some(replaced) = replace_term(&out, from, to) {
            out = replaced;
            changed = true;
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_query_gets_english_variant() {
        let variants = cross_lingual_variants("BM25 排序算法", 2);
        assert_eq!(variants, vec!["BM25 ranking algorithm".to_string()]);
        assert!(!variants.iter().any(|v| v == "BM25 排序算法"));
    }

    #[test]
    fn english_query_gets_chinese_variant() {
        let variants = cross_lingual_variants("vector search performance", 2);
        assert!(variants
            .iter()
            .any(|v| v.contains("向量") || v.contains("性能")));
    }

    #[test]
    fn unknown_terms_and_other_scripts_produce_nothing() {
        assert!(cross_lingual_variants("totally unrelated gibberish", 2).is_empty());
        assert!(cross_lingual_variants(" completely unrelated ", 0).is_empty());
        assert!(cross_lingual_variants("Привет мир", 2).is_empty());
    }

    #[test]
    fn longest_term_wins_and_result_is_bounded() {
        let variants = cross_lingual_variants("中文分词 和 语义缓存 检索", 1);
        assert_eq!(variants.len(), 1);
        assert!(variants[0].contains("chinese tokenization"));
    }

    #[test]
    fn full_variant_translates_every_known_term() {
        let variant = full_cross_lingual_variant("语义缓存 与 向量检索").unwrap();
        assert_eq!(variant, "semantic cache 与 vector search");
        assert!(full_cross_lingual_variant("nothing known here").is_none());
    }
}
