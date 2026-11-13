//! Token estimation and UTF-8-safe truncation for Context Assembly.
//!
//! An estimator turns a text fragment into an approximate token count. The
//! assembler only ever uses it through this trait, so a caller can plug in a
//! real BPE tokenizer later without changing the assembly invariants.
//!
//! **Contract:** `estimate()` must be monotone non-decreasing over text
//! prefixes. Both built-in estimators satisfy this; `truncate_to_tokens`
//! relies on it for its binary search.

/// Pluggable token-cost function for evidence text and citation references.
pub trait TokenEstimator {
    /// Estimated number of tokens in `text`.
    fn estimate(&self, text: &str) -> usize;
}

/// A CJK-aware heuristic estimator for offline / default use.
///
/// CJK and full-width characters count as one token each; each
/// whitespace-delimited run of other characters costs `ceil(len / 4)`
/// tokens (i.e. one token per four characters, plus one for a remainder).
/// This deliberately over-estimates rather than under-estimates: a context
/// assembly that exceeds the caller's budget is a correctness bug, while a
/// conservative assembly is merely slightly smaller.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicEstimator;

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, text: &str) -> usize {
        let mut tokens = 0usize;
        let mut run = 0usize;
        for ch in text.chars() {
            if is_wide(ch) {
                flush_run(&mut run, &mut tokens);
                tokens += 1;
            } else if ch.is_whitespace() {
                flush_run(&mut run, &mut tokens);
            } else {
                run += 1;
                if run == 4 {
                    tokens += 1;
                    run = 0;
                }
            }
        }
        flush_run(&mut run, &mut tokens);
        tokens
    }
}

fn flush_run(run: &mut usize, tokens: &mut usize) {
    if *run > 0 {
        *tokens += 1;
        *run = 0;
    }
}

fn is_wide(ch: char) -> bool {
    matches!(ch,
        '\u{1100}'..='\u{115F}' | '\u{2E80}'..='\u{A4CF}' | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}' | '\u{FE10}'..='\u{FE6F}' | '\u{FF00}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}' | '\u{20000}'..='\u{3FFFD}')
}

/// Whitespace-delimited word-count estimator.
///
/// Intended for deterministic tests and for callers whose tokenizer is close
/// to a word count. It is monotone over prefixes, so it can drive truncation.
#[derive(Debug, Clone, Copy, Default)]
pub struct WordCountEstimator;

impl TokenEstimator for WordCountEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

/// Fixed cost per non-empty fragment. Useful for budget-boundary tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedCostEstimator(pub usize);

impl TokenEstimator for FixedCostEstimator {
    fn estimate(&self, text: &str) -> usize {
        if text.is_empty() {
            0
        } else {
            self.0
        }
    }
}

/// Truncates `text` to at most `max_tokens` **without altering its content**.
///
/// The result is always a verbatim UTF-8-safe prefix of the original text —
/// never a summary, never a paraphrase, and never a trailing ellipsis that
/// could be mistaken for original wording. Callers report truncation through
/// `AssembledEvidence::truncated` instead.
///
/// Requires `estimator` to be monotone non-decreasing over prefixes.
pub fn truncate_to_tokens(text: &str, max_tokens: usize, estimator: &dyn TokenEstimator) -> String {
    if max_tokens == 0 || text.is_empty() {
        return String::new();
    }
    if estimator.estimate(text) <= max_tokens {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let prefix: String = chars[..mid].iter().collect();
        if estimator.estimate(&prefix) <= max_tokens {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_counts_cjk_per_character() {
        assert_eq!(HeuristicEstimator.estimate("中文检索"), 4);
    }

    #[test]
    fn heuristic_charges_ceil_len_over_four_per_ascii_run() {
        // ceil(5/4) = 2 tokens per run, two runs -> 4.
        assert_eq!(HeuristicEstimator.estimate("hello world"), 4);
        assert_eq!(HeuristicEstimator.estimate("hello"), 2);
        assert_eq!(HeuristicEstimator.estimate("abcd"), 1);
    }

    #[test]
    fn heuristic_over_estimates_mixed_text() {
        // 2 CJK chars + one 4-char ASCII run + one short run.
        assert!(HeuristicEstimator.estimate("中文 rust db") >= 4);
    }

    #[test]
    fn truncation_is_a_verbatim_prefix_and_char_boundary_safe() {
        let text = "中文检索系统 alpha beta gamma delta";
        let out = truncate_to_tokens(text, 4, &HeuristicEstimator);
        assert!(text.starts_with(&out));
        assert!(HeuristicEstimator.estimate(&out) <= 4);
        assert!(!out.is_empty());
    }

    #[test]
    fn zero_budget_yields_empty_not_a_summary() {
        assert_eq!(truncate_to_tokens("anything", 0, &HeuristicEstimator), "");
    }

    #[test]
    fn word_count_truncation_never_exceeds_budget() {
        let text = "one two three four five six";
        let out = truncate_to_tokens(text, 3, &WordCountEstimator);
        // A verbatim prefix may retain the trailing space before the next
        // word; that is deliberate — the output is never re-worded.
        assert_eq!(out.trim_end(), "one two three");
        assert_eq!(WordCountEstimator.estimate(&out), 3);
        assert!(text.starts_with(&out));
    }
}
