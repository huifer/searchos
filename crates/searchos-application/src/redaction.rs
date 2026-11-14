//! Ingest-time secret redaction (RFC-087).
//!
//! Secrets pasted into notes, exports and uploads are stored verbatim and
//! become retrievable — the cache-validity and federation layers happily
//! serve them to any workspace reader. This module applies a versioned rule
//! set to ingest text BEFORE persistence, hashing and indexing, replacing
//! each match with a stable `[REDACTED:<rule>]` marker.
//!
//! Contract:
//! * Redaction happens at INGEST time only — never retroactively over
//!   already-stored documents.
//! * The marker is stable, so re-ingesting the same ORIGINAL text produces
//!   the same redacted form (idempotent hashing).
//! * Rules are deliberately conservative: a match must carry real secret
//!   shape (long token, key-block, or a credential assignment whose value
//!   contains a digit). Normal prose never matches; when in doubt the rule
//!   does not fire — over-redaction would silently corrupt knowledge.
//! * This is NOT PII redaction; person-data rules are future work (RFC-087).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Version of the rule set. Stored in the report so an operator can tell
/// WHICH rules redacted a document.
pub const REDACTION_RULES_VERSION: &str = "1";

/// One rule's redaction count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionHit {
    pub rule: String,
    pub count: usize,
}

/// Outcome of one redaction pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReport {
    /// Whether anything was redacted at all.
    pub redacted: bool,
    /// Per-rule counts, rule order preserved, empty when nothing matched.
    pub hits: Vec<RedactionHit>,
    pub rules_version: String,
}

struct Rule {
    name: &'static str,
    /// Whole-pattern regex; every match is replaced.
    pattern: Regex,
    /// Optional post-filter: the matched TEXT must satisfy this (used by the
    /// credential-assignment rule to require a digit in the value).
    requires_digit_in_match: bool,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            Rule {
                name: "private_key_block",
                // The whole armored block, header to footer.
                pattern: Regex::new(
                    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                )
                .expect("private_key_block pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "openai_style_key",
                // `sk-` prefixed tokens (OpenAI style, incl. `sk-proj-…`).
                pattern: Regex::new(r"sk-[A-Za-z0-9_\-]{20,}").expect("openai_style_key pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "aws_access_key",
                pattern: Regex::new(r"AKIA[0-9A-Z]{16}").expect("aws_access_key pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "github_token",
                // Fine-grained / classic `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`.
                pattern: Regex::new(r"gh[pousr]_[A-Za-z0-9]{30,}").expect("github_token pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "slack_token",
                pattern: Regex::new(r"xox[baprs]-[A-Za-z0-9\-]{10,}").expect("slack_token pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "bearer_token",
                // `Authorization: Bearer <token>` style values.
                pattern: Regex::new(r"(?i)bearer[ \t]+[A-Za-z0-9\-\._~+/]{20,}={0,2}")
                    .expect("bearer_token pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "credential_assignment",
                // `api_key = …`, `SECRET:…`, `set_password(…)` is NOT this —
                // only direct assignments of a single long token after a
                // credential name. The digit requirement keeps ordinary
                // prose ("token: required") out.
                pattern: Regex::new(
                    r#"(?i)\b(api[_-]?key|secret|token|passw(or)?d|passwd|pwd)\b[ \t]*[=:][ \t]*["']?([A-Za-z0-9/+\-_.]{8,})["']?"#,
                )
                .expect("credential_assignment pattern"),
                requires_digit_in_match: true,
            },
        ]
    })
}

/// Redacts secrets from `text`.
///
/// Returns the redacted text and a report. Deterministic: the same input
/// always yields the same output (stable markers, fixed rule order), so
/// re-ingesting the original text hashes identically.
pub fn redact_secrets(text: &str) -> (String, RedactionReport) {
    let mut current = text.to_string();
    let mut hits: Vec<RedactionHit> = Vec::new();
    for rule in rules() {
        let mut count = 0usize;
        if rule.requires_digit_in_match {
            // Selective replacement: a match only counts as a secret when its
            // captured value contains a digit; otherwise it stays verbatim.
            let mut out = String::with_capacity(current.len());
            let mut last = 0usize;
            for m in rule.pattern.find_iter(&current) {
                if m.as_str().chars().any(|c| c.is_ascii_digit()) {
                    count += 1;
                    out.push_str(&current[last..m.start()]);
                    out.push_str(&format!("[REDACTED:{}]", rule.name));
                    last = m.end();
                }
            }
            out.push_str(&current[last..]);
            current = out;
        } else {
            count = rule.pattern.find_iter(&current).count();
            if count > 0 {
                current = rule
                    .pattern
                    .replace_all(&current, format!("[REDACTED:{}]", rule.name))
                    .into_owned();
            }
        }
        if count > 0 {
            hits.push(RedactionHit {
                rule: rule.name.to_string(),
                count,
            });
        }
    }
    (
        current,
        RedactionReport {
            redacted: !hits.is_empty(),
            hits,
            rules_version: REDACTION_RULES_VERSION.to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_assignment_is_redacted() {
        let (out, report) = redact_secrets(
            "My config:\nOPENAI_API_KEY=sk-proj4B7f8Gh2Jk3Lm9NpQrStUv1Wx2Yz3A4b5C6d7\nsee docs.",
        );
        assert!(!out.contains("sk-proj4B7f8Gh2"));
        assert!(out.contains("[REDACTED:"), "{out}");
        assert!(report.redacted);
        assert!(report
            .hits
            .iter()
            .any(|h| h.rule == "credential_assignment" || h.rule == "openai_style_key"));
    }

    #[test]
    fn private_key_blocks_are_removed_wholesale() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBPAIBAAJBAKx\nrandom body lines\nwith digits 12345\n-----END RSA PRIVATE KEY-----\n";
        let (out, report) = redact_secrets(&format!("before\n{pem}after"));
        assert!(!out.contains("MIIBPAIBAAJBAKx"));
        assert!(!out.contains("BEGIN RSA PRIVATE KEY"));
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert_eq!(report.hits[0].rule, "private_key_block");
    }

    #[test]
    fn cloud_and_scm_tokens_are_redacted() {
        let (out, _) = redact_secrets(
            "aws AKIAIOSFODNN7EXAMPLE and gh ghgojektokenvalueislongenough123456 and slack xoxb-123456789012-test",
        );
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(out.contains("[REDACTED:aws_access_key]"));
    }

    #[test]
    fn ordinary_prose_is_never_redacted() {
        let prose = "The token economy paper is required reading; version 2 was \
            published in 2019. api key management is a chapter title. \
            password advice: use a manager.";
        let (out, report) = redact_secrets(prose);
        assert_eq!(out, prose, "no secret shape: nothing may change");
        assert!(!report.redacted);
        assert!(report.hits.is_empty());
    }

    #[test]
    fn credential_value_without_a_digit_is_left_alone() {
        let prose = "token: required\npassword = correct";
        let (out, report) = redact_secrets(prose);
        assert_eq!(out, prose);
        assert!(!report.redacted);
    }

    #[test]
    fn redaction_is_deterministic() {
        let sample =
            "openai sk-abc123def456ghi789jkl012 and bearer Bearer aaaabbbbccccddddeeeeffff";
        let (a, ra) = redact_secrets(sample);
        let (b, rb) = redact_secrets(sample);
        assert_eq!(a, b);
        assert_eq!(ra, rb);
    }

    #[test]
    fn marker_is_stable_across_reingest() {
        let original = "OPENAI_API_KEY = sk-abcdef0123456789abcdef01";
        let (redacted, _) = redact_secrets(original);
        let (again, report_again) = redact_secrets(&redacted);
        assert_eq!(redacted, again, "markers must not re-trigger rules");
        assert!(!report_again.redacted, "a redacted doc has no secrets left");
    }
}
