//! Query expansion (RFC-003 3.6).
//!
//! Query Expansion 不是默认开启 — it only triggers on a coverage gap.

use serde::{Deserialize, Serialize};

/// Expansion policy limits (RFC-003 3.6 initial config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpansionConfig {
    pub enabled: bool,
    pub max_expansions: u32,
    pub max_external_queries: u32,
    /// Only expand when a coverage gap is proven.
    pub require_coverage_gap: bool,
}

impl Default for ExpansionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_expansions: 3,
            max_external_queries: 5,
            require_coverage_gap: true,
        }
    }
}

/// Rule-based query expander.
#[derive(Debug, Clone, Default)]
pub struct QueryExpander {
    pub config: ExpansionConfig,
}

impl QueryExpander {
    /// New expander with custom config.
    pub fn new(config: ExpansionConfig) -> Self {
        Self { config }
    }

    /// Whether expansion is permitted for this request.
    pub fn should_expand(&self, coverage_gap: bool, external_queries_so_far: u32) -> bool {
        if !self.config.enabled {
            return false;
        }
        if self.config.require_coverage_gap && !coverage_gap {
            return false;
        }
        external_queries_so_far < self.config.max_external_queries
    }

    /// Generates up to `max_expansions` variants of a base query for an
    /// uncovered sub-question (e.g. entity-focused reformulations).
    pub fn expand(&self, base: &str, gap_question: &str) -> Vec<String> {
        let mut out = Vec::new();
        if !self.should_expand(true, 0) {
            return out;
        }
        let gap = gap_question.trim();
        if !gap.is_empty() && !base.contains(gap) {
            out.push(format!("{base} {gap}"));
        }
        out.push(gap.to_string());
        // Entity-comparison reformulation for "A vs B" style gaps.
        for kw in ["documentation", "official", "release notes"] {
            let v = format!("{gap} {kw}");
            if !out.contains(&v) {
                out.push(v);
            }
        }
        out.truncate(self.config.max_expansions as usize);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_expands() {
        let e = QueryExpander::new(ExpansionConfig {
            enabled: false,
            ..Default::default()
        });
        assert!(e.expand("rust frameworks", "pricing").is_empty());
        assert!(!e.should_expand(true, 0));
    }

    #[test]
    fn no_gap_no_expansion_by_default() {
        let e = QueryExpander::default();
        assert!(!e.should_expand(false, 0));
        assert!(e.should_expand(true, 0));
    }

    #[test]
    fn respects_max_expansions() {
        let e = QueryExpander::default();
        let v = e.expand("compare tokio axum", "tokio pricing");
        assert_eq!(v.len(), 3);
    }
}

/// Canonical synonym table (RFC-072).
///
/// One table for the whole system: the memory service normalizes with the
/// same pairs, and query expansion generates ALTERNATIVE phrasings from it.
/// The table is deliberately small and domain-neutral — a bigger one belongs
/// in a curated resource, not in code.
pub const SYNONYMS: &[(&str, &[&str])] = &[
    ("async", &["asynchronous", "non-blocking"]),
    ("asynchronous", &["async", "non-blocking"]),
    ("runtime", &["executor", "scheduler"]),
    ("executor", &["runtime", "scheduler"]),
    ("scheduler", &["runtime", "executor"]),
    ("coroutine", &["task", "async task"]),
    ("coroutines", &["task", "async task"]),
    ("db", &["database"]),
    ("database", &["db"]),
    ("k8s", &["kubernetes"]),
    ("kubernetes", &["k8s"]),
    ("js", &["javascript"]),
    ("javascript", &["js"]),
    ("ml", &["machine learning"]),
    ("machine learning", &["ml"]),
    ("auth", &["authentication", "authorization"]),
    ("config", &["configuration", "settings"]),
    ("configuration", &["config", "settings"]),
    ("perf", &["performance"]),
    ("performance", &["perf"]),
    ("api", &["interface", "endpoint"]),
];

/// Synonyms for one term (empty when the term is not in the table).
pub fn synonyms_for(term: &str) -> &'static [&'static str] {
    let needle = term.to_lowercase();
    SYNONYMS
        .iter()
        .find(|(key, _)| *key == needle)
        .map(|(_, values)| *values)
        .unwrap_or(&[])
}

/// RFC-072: alternative phrasings of a query via the synonym table.
///
/// At most `max` variants are returned, each replacing ONE known term with
/// its first synonym. A variant that already appears in the query is
/// skipped (it would search for the same thing).
pub fn synonym_variants(query: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = query.to_lowercase();
    for term in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        for synonym in synonyms_for(term) {
            let variant = replace_token(query, term, synonym);
            if variant.is_empty() || variant == query {
                continue;
            }
            if lower.contains(&synonym.to_lowercase()) {
                continue;
            }
            if !out.contains(&variant) {
                out.push(variant);
            }
            if out.len() >= max {
                return out;
            }
        }
    }
    out
}

/// Replaces a whole token in the ORIGINAL query (case-insensitive),
/// preserving the surrounding whitespace and punctuation.
fn replace_token(query: &str, token: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(query.len() + replacement.len());
    let mut replaced = false;
    for word in query.split_inclusive(char::is_whitespace) {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !replaced && trimmed.eq_ignore_ascii_case(token) {
            // Reinsert the whole word with the token swapped, so trailing
            // spaces and punctuation survive ("async runtime" must never
            // become "asynchronousruntime").
            out.push_str(&word.replacen(trimmed, replacement, 1));
            replaced = true;
        } else {
            out.push_str(word);
        }
    }
    out.trim().to_string()
}

/// RFC-072: sub-question variants for QUESTION-shaped queries.
///
/// "how to configure tokio" → "configure tokio", "configure tokio example",
/// "configure tokio documentation". Non-question queries produce nothing:
/// inventing variants for a keyword query only dilutes the budget.
pub fn sub_question_variants(query: &str, max: usize) -> Vec<String> {
    let trimmed = query.trim();
    let lower = trimmed.to_lowercase();
    const PREFIXES: [&str; 10] = [
        "how to ",
        "how do i ",
        "how do we ",
        "how can i ",
        "what is ",
        "what are ",
        "why is ",
        "why does ",
        "explain ",
        "what does ",
    ];
    let mut base = None;
    for prefix in PREFIXES {
        if lower.starts_with(prefix) {
            base = Some(trimmed[prefix.len()..].trim().to_string());
            break;
        }
    }
    let Some(base) = base else {
        return Vec::new();
    };
    if base.is_empty() || base == trimmed {
        return Vec::new();
    }
    let mut out = vec![base.clone()];
    if max > 1 {
        out.push(format!("{base} example"));
    }
    if max > 2 {
        out.push(format!("{base} documentation"));
    }
    out.truncate(max);
    out
}

#[cfg(test)]
mod rfc072_tests {
    use super::*;

    #[test]
    fn synonym_variants_replace_known_terms() {
        let variants = synonym_variants("tokio async runtime guide", 2);
        assert!(
            variants.iter().any(|v| v.contains("asynchronous")),
            "{variants:?}"
        );
        assert!(variants.iter().all(|v| v.contains("tokio")), "{variants:?}");
    }

    #[test]
    fn synonym_variants_skip_queries_that_already_contain_the_synonym() {
        // "asynchronous" already covers "async" — no pointless variant.
        let variants = synonym_variants("asynchronous programming", 2);
        assert!(
            !variants.iter().any(|v| v.contains("async ")),
            "{variants:?}"
        );
    }

    #[test]
    fn token_replacement_keeps_word_boundaries() {
        let variants = synonym_variants("tokio async runtime", 2);
        assert!(
            variants.iter().any(|v| v == "tokio asynchronous runtime"),
            "{variants:?}"
        );
        assert!(
            variants.iter().all(|v| !v.contains("asynchronousruntime")),
            "{variants:?}"
        );
    }

    #[test]
    fn unknown_terms_produce_no_variants() {
        assert!(synonym_variants("zzzqqq wwwxxx", 3).is_empty());
    }

    #[test]
    fn sub_question_variants_only_for_questions() {
        let variants = sub_question_variants("how to configure tokio", 3);
        assert_eq!(variants[0], "configure tokio");
        assert!(variants.iter().any(|v| v.contains("example")));
        assert!(variants.iter().any(|v| v.contains("documentation")));
        // A keyword query is not a question: no variants.
        assert!(sub_question_variants("tokio runtime", 3).is_empty());
    }

    #[test]
    fn variants_are_bounded_and_deterministic() {
        let a = synonym_variants("tokio async runtime", 2);
        let b = synonym_variants("tokio async runtime", 2);
        assert_eq!(a, b);
        assert!(a.len() <= 2);
    }

    #[test]
    fn cjk_queries_are_left_alone() {
        assert!(synonym_variants("本地优先检索引擎", 2).is_empty());
        assert!(sub_question_variants("如何配置本地优先检索引擎", 3).is_empty());
    }
}
