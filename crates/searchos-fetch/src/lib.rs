//! Web fetch layer (RFC-010, RFC-014).
//!
//! - [`guard`] — SSRF protection (RFC-014 14.1): localhost / private v4+v6 /
//!   cloud metadata blocked, DNS resolution validated, every redirect hop
//!   re-checked, bounded redirects, response size and timeouts.
//! - [`robots`] — robots.txt parsing + ROBOTS_DISALLOWED.
//! - [`extract`] — HTML → title / markdown-ish text extraction.
//! - [`http`] — fetcher with conditional GET (ETag / If-Modified-Since).

pub mod extract;
pub mod guard;
pub mod http;
pub mod robots;

pub use guard::{UrlGuard, UrlViolation};
pub use http::{FetchLimits, FetchOutcome, HttpFetcher};
pub use robots::{RobotsChecker, RobotsDecision};
