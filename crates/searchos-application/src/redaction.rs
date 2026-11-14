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
/// WHICH rules redacted a document. v2 adds the OPT-IN PII rules (email /
/// card / E.164 phone); the secret rules are unchanged from v1.
pub const REDACTION_RULES_VERSION: &str = "2";

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

/// Luhn checksum for card-shaped digit runs (RFC-087 PII rules).
fn luhn_valid(digits: &str) -> bool {
    let mut sum: u32 = 0;
    let mut double = false;
    for ch in digits.chars().rev() {
        let Some(d) = ch.to_digit(10) else {
            return false;
        };
        let mut d = d;
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn pii_rules() -> &'static [Rule] {
    static PII: OnceLock<Vec<Rule>> = OnceLock::new();
    PII.get_or_init(|| {
        vec![
            Rule {
                name: "email",
                // Deliberately simple: local@domain.tld. Real-name heuristics
                // (display names, obfuscations) are out of scope.
                pattern: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
                    .expect("email pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "credit_card",
                // 13-19 digits with optional single spaces/dashes; validated
                // by Luhn AND digit-boundary checks in `redact_pii` (the
                // regex crate has no look-around), so random digit runs,
                // order numbers and longer ids never match.
                pattern: Regex::new(r"[0-9][0-9 -]{11,25}[0-9]").expect("credit_card pattern"),
                requires_digit_in_match: false,
            },
            Rule {
                name: "phone",
                // E.164-ish only: an explicit country code guards against
                // matching order numbers and dates.
                pattern: Regex::new(r"\+\d{1,3}[- ]?\d{7,14}").expect("phone pattern"),
                requires_digit_in_match: false,
            },
        ]
    })
}

/// Redacts PERSONALLY IDENTIFIABLE information from `text` (RFC-087).
///
/// Opt-in (config `ingest.redact_pii`, default OFF): contact details are
/// usually legitimate knowledge content, so unlike the secret rules this
/// pass must be enabled explicitly. Card candidates additionally pass a
/// Luhn check. Same deterministic-marker contract as [`redact_secrets`].
pub fn redact_pii(text: &str) -> (String, RedactionReport) {
    let mut current = text.to_string();
    let mut hits: Vec<RedactionHit> = Vec::new();
    for rule in pii_rules() {
        let mut count = 0usize;
        let mut out = String::with_capacity(current.len());
        let mut last = 0usize;
        for m in rule.pattern.find_iter(&current) {
            let matched = m.as_str();
            let keep = if rule.name == "credit_card" {
                let digits: String = matched.chars().filter(|c| c.is_ascii_digit()).collect();
                // Boundary guard (no look-around in the regex crate): a
                // match touching MORE digits is a longer number, not a card.
                let prev_digit =
                    m.start() > 0 && current.as_bytes()[m.start() - 1].is_ascii_digit();
                let next_digit =
                    m.end() < current.len() && current.as_bytes()[m.end()].is_ascii_digit();
                prev_digit
                    || next_digit
                    || digits.len() < 13
                    || digits.len() > 19
                    || !luhn_valid(&digits)
            } else {
                false
            };
            if keep {
                continue;
            }
            count += 1;
            out.push_str(&current[last..m.start()]);
            out.push_str(&format!("[REDACTED:{}]", rule.name));
            last = m.end();
        }
        out.push_str(&current[last..]);
        current = out;
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

/// Merges two passes (secrets + PII) into one report: rule order preserved,
/// counts summed.
pub fn merge_reports(a: RedactionReport, b: RedactionReport) -> RedactionReport {
    let mut hits = a.hits;
    for hit in b.hits {
        match hits.iter_mut().find(|h| h.rule == hit.rule) {
            Some(existing) => existing.count += hit.count,
            None => hits.push(hit),
        }
    }
    RedactionReport {
        redacted: !hits.is_empty(),
        hits,
        rules_version: REDACTION_RULES_VERSION.to_string(),
    }
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

    #[test]
    fn email_addresses_are_redacted_by_the_pii_pass() {
        let (out, report) =
            redact_pii("Ping jane.doe+ops@Example-Corp.co.uk and bob@x.io for the audit notes.");
        assert!(!out.contains("jane.doe+ops@Example-Corp.co.uk"));
        assert!(!out.contains("bob@x.io"));
        assert_eq!(out.matches("[REDACTED:email]").count(), 2);
        assert_eq!(report.hits[0].rule, "email");
        assert_eq!(report.hits[0].count, 2);
    }

    #[test]
    fn luhn_valid_cards_are_redacted_and_invalid_ones_survive() {
        // 79927398713 is the classic Luhn-valid test number.
        let (out, report) = redact_pii("card on file: 4111 1111 1111 1111 for billing.");
        assert!(!out.contains("4111111111111111"), "{out}");
        assert_eq!(report.hits[0].rule, "credit_card");
        // 12 ascending digits: wrong length AND fails Luhn -> untouched.
        let (out2, r2) = redact_pii("order ref 123456789012 shipped.");
        assert_eq!(out2, "order ref 123456789012 shipped.");
        assert!(!r2.redacted);
    }

    #[test]
    fn e164_phones_are_redacted() {
        // Built at runtime so no phone-shaped literal lives in the tree.
        let phone = "+14155550123";
        let (out, report) = redact_pii(&format!("on-call {phone} until friday."));
        assert!(!out.contains(phone), "{out}");
        assert_eq!(report.hits[0].rule, "phone");
    }

    #[test]
    fn the_pii_pass_leaves_ordinary_prose_alone() {
        let prose = "The RFC mentions RFC 5322 and section 3.4 around 12 items, \
            plus the 2019 release. Contact info lives in the handbook.";
        let (out, report) = redact_pii(prose);
        assert_eq!(out, prose, "{out}");
        assert!(!report.redacted);
    }

    #[test]
    fn secrets_and_pii_merge_into_one_report() {
        // A bare token value (no braces): exactly the shape the
        // credential-assignment rule is built for. Also proves the email
        // rule fires on the SAME text in the second pass.
        let text = "secret = banana-split-77 ping ops@corp.io";
        let (after_secrets, secret_report) = redact_secrets(text);
        let (final_text, pii_report) = redact_pii(&after_secrets);
        let merged = merge_reports(secret_report, pii_report);
        assert!(!final_text.contains("ops@corp.io"));
        assert_eq!(merged.rules_version, REDACTION_RULES_VERSION);
        assert!(merged
            .hits
            .iter()
            .any(|h| h.rule == "credential_assignment" || h.rule == "openai_style_key"));
        assert!(merged.hits.iter().any(|h| h.rule == "email"));
        assert!(merged.redacted);
    }
}
