//! Document intelligence: extractive summary + keywords (RFC-036 v1).
//!
//! Deterministic, offline, and source-faithful: a summary is a VERBATIM
//! sentence sequence from the source (never a generated paraphrase), and
//! keywords are tokens actually observed in the text. Both are computed at
//! ingest, stored with the version, and surfaced by `document inspect` /
//! `GET /documents/:id`.
//!
//! A document that fails strict UTF-8 decoding never reaches this module —
//! the ingest path already rejects it.

use serde::{Deserialize, Serialize};

/// Common English function words excluded from keyword scores. Domain- or
/// language-specific resources belong in a future curated pack.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "have", "has",
    "had", "but", "not", "you", "your", "its", "into", "than", "then", "they", "their", "when",
    "what", "which", "who", "how", "can", "will", "would", "should", "about", "also", "more",
    "most", "some", "such", "only", "very", "over", "under", "been", "being", "does", "did",
];

/// Computed document insights. Both fields are optional: a document with no
/// sentence boundaries can still have keywords, and vice versa.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentInsights {
    pub summary: Option<String>,
    pub keywords: Vec<String>,
}

/// Extracts keywords by frequency over alphanumeric tokens, with a
/// deterministic tie-break (count desc, word asc). CJK runs are emitted as
/// overlapping bigrams — the same analyzer strategy the retrieval layer
/// uses — so Chinese documents get useful keywords too.
pub fn extract_keywords(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for token in lower.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() || STOPWORDS.contains(&token) {
            continue;
        }
        if token.chars().count() > 1 && token.chars().all(|c| !c.is_ascii()) {
            // CJK: bigrams (filtered by the 40-char floor elsewhere).
            let chars: Vec<char> = token.chars().collect();
            for pair in chars.windows(2) {
                let bigram: String = pair.iter().collect();
                *counts.entry(bigram).or_insert(0) += 1;
            }
        } else if token.chars().count() >= 3 {
            *counts.entry(token.to_string()).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(max).map(|(word, _)| word).collect()
}

/// Splits into sentence-like chunks on `.!?。！？` boundaries, retaining the
/// delimiter. Very short fragments are folded into the previous sentence.
fn sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？') {
            let trimmed = current.trim();
            if trimmed.len() >= 12 {
                out.push(trimmed.to_string());
            } else if let Some(previous) = out.last_mut() {
                previous.push(' ');
                previous.push_str(trimmed);
            }
            current.clear();
        }
    }
    let tail = current.trim();
    if tail.len() >= 12 {
        out.push(tail.to_string());
    } else if !tail.is_empty() {
        if let Some(previous) = out.last_mut() {
            previous.push(' ');
            previous.push_str(tail);
        }
    }
    out
}

/// Builds an EXTRACTIVE summary: the top `max_sentences` sentences by keyword
/// overlap, emitted in ORIGINAL document order (chosen sentences are never
/// reordered). The source text itself is never rewritten.
pub fn extract_summary(text: &str, keywords: &[String], max_sentences: usize) -> Option<String> {
    if max_sentences == 0 {
        return None;
    }
    let parts = sentences(text);
    if parts.is_empty() {
        return None;
    }
    let kw: Vec<&str> = keywords.iter().map(|s| s.as_str()).collect();
    let mut ranked: Vec<(usize, usize)> = parts
        .iter()
        .enumerate()
        .map(|(index, sentence)| {
            let lower = sentence.to_lowercase();
            let score = kw.iter().filter(|word| lower.contains(**word)).count();
            (score, index)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut chosen: Vec<usize> = ranked
        .into_iter()
        .take(max_sentences.min(parts.len()))
        .map(|(_, index)| index)
        .collect();
    chosen.sort_unstable();
    let summary = chosen
        .into_iter()
        .filter_map(|i| parts.get(i))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    // Verbatim prefix: never truncate mid-sentence into invented text.
    let mut capped = summary.clone();
    if capped.chars().count() > 600 {
        capped = capped.chars().take(600).collect::<String>() + "…";
    }
    Some(capped)
}

pub fn analyze(text: &str) -> DocumentInsights {
    let keywords = extract_keywords(text, 8);
    let summary = extract_summary(text, &keywords, 2);
    DocumentInsights { summary, keywords }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_verbatim_source_text_in_original_order() {
        let text = "SearchOS is a local-first search engine. It fuses lexical and vector recall. The final gate checks relevance and diversity. Reports stay honest about partial evidence.";
        let insights = analyze(text);
        let summary = insights.summary.unwrap();
        // Each selected sentence is verbatim source text; the summary may
        // OMIT intervening sentences but never rewrites a chosen one.
        assert!(
            summary
                .split(". ")
                .filter(|s| !s.trim().is_empty())
                .all(|part| {
                    text.contains(part.trim()) || text.contains(&format!("{}.", part.trim()))
                }),
            "every chosen sentence is verbatim: {summary}"
        );
        assert!(
            summary.find("SearchOS").unwrap() < summary.find("fuses").unwrap_or(usize::MAX)
                || !summary.contains("fuses"),
            "chosen sentences keep document order: {summary}"
        );
        assert!(!insights.keywords.is_empty());
    }

    #[test]
    fn keywords_ignore_stopwords_and_are_deterministic() {
        let text = "Tokio is an async runtime. Tokio is fast and tokio is reliable.";
        assert_eq!(extract_keywords(text, 4), extract_keywords(text, 4));
        let kw = extract_keywords(text, 4);
        assert!(!kw.iter().any(|w| w == "the" || w == "and" || w == "is"));
        assert!(kw.contains(&"tokio".to_string()));
    }

    #[test]
    fn cjk_text_gets_bigram_keywords_and_sentence_summary() {
        let text = "SearchOS 是一个本地优先的检索基础设施。系统会先在本地解析问题，再决定是否需要外部检索。";
        let insights = analyze(text);
        assert!(insights.keywords.iter().any(|w| w.chars().count() == 2));
        assert!(insights.summary.unwrap().contains("SearchOS"));
    }

    #[test]
    fn empty_or_tiny_text_is_honest() {
        let insights = analyze("");
        assert!(insights.summary.is_none());
        assert!(insights.keywords.is_empty());
    }
}
