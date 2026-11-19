//! RFC-032: publisher-level trust scoring.
//!
//! Source-level reliability answers "did this provider return usable
//! results?"; this answers "when this PUBLISHER's pages were judged, how
//! often were they actually relevant?". It is a derived read model over the
//! append-only feedback stream — no new table, no write path, no invented
//! labels.
//!
//! A publisher with fewer than [`MIN_PUBLISHER_TRUST_SAMPLES`] judgements is
//! UNKNOWN, not untrustworthy: `trust` is `None` and the caller must not
//! treat it as zero. Only `canonical` / `helpful` judgements count as
//! relevant; `related` is related, not an answer.

use serde::{Deserialize, Serialize};

/// Judgements a publisher needs before a trust score is reported.
pub const MIN_PUBLISHER_TRUST_SAMPLES: u32 = 2;

/// Trust score for one publisher host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublisherTrust {
    /// Canonical host (`source_host`), never a raw URL.
    pub publisher: String,
    /// Judgements observed for this publisher.
    pub judged: u32,
    /// Judgements whose verdict was `canonical` or `helpful`.
    pub relevant: u32,
    /// `relevant / judged` once the sample floor is met; `None` below it.
    pub trust: Option<f32>,
}

impl PublisherTrust {
    pub fn new(publisher: impl Into<String>, judged: u32, relevant: u32) -> Self {
        let publisher = publisher.into();
        let relevant = relevant.min(judged);
        let trust = if judged >= MIN_PUBLISHER_TRUST_SAMPLES {
            Some((relevant as f32 / judged as f32).clamp(0.0, 1.0))
        } else {
            None
        };
        Self {
            publisher,
            judged,
            relevant,
            trust,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_none_below_the_sample_floor() {
        let row = PublisherTrust::new("example.com", 1, 1);
        assert_eq!(row.trust, None, "one judgement is not evidence");
    }

    #[test]
    fn trust_is_the_relevant_fraction_once_judged_enough() {
        let row = PublisherTrust::new("example.com", 4, 3);
        assert_eq!(row.trust, Some(0.75));
        assert_eq!(row.judged, 4);
        assert_eq!(row.relevant, 3);
    }

    #[test]
    fn relevant_never_exceeds_judged() {
        let row = PublisherTrust::new("example.com", 2, 9);
        assert_eq!(row.relevant, 2);
        assert_eq!(row.trust, Some(1.0));
    }
}
