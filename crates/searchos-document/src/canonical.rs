//! One canonical-URL implementation for fetch, ingest, merge and index writes
//! (Part 30 INV-30-15).
//!
//! The raw URL remains available for display; canonicalisation lowercases only
//! the scheme/host, preserves case-sensitive path segments, removes fragments
//! and tracking parameters, and orders the remaining query parameters.

use url::form_urlencoded::Serializer;
use url::Url;

/// Query parameters that never change document identity.
const TRACKING_KEYS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "fbclid",
    "gclid",
    "ref",
    "ref_src",
];

/// Returns the canonical identity of a URL.
///
/// `searchos-local://<workspace>/<source_key>` is treated as a normal URI with
/// a preserved, case-sensitive path. Invalid / non-URI strings fall back to a
/// conservative trim that never lowercases the body.
pub fn canonical_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed).trim();
    if without_fragment.is_empty() {
        return String::new();
    }
    let Ok(mut url) = Url::parse(without_fragment) else {
        return fallback(without_fragment);
    };
    url.set_fragment(None);
    // Keep meaningful query parameters but sort them for stable identity.
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .into_owned()
        .filter(|(k, _)| {
            let key = k.to_ascii_lowercase();
            !TRACKING_KEYS.contains(&key.as_str()) && !key.starts_with("utm_")
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        let query = Serializer::new(String::new()).extend_pairs(pairs).finish();
        url.set_query(Some(&query));
    }
    let path = url.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        url.set_path(path.trim_end_matches('/'));
    }
    let mut out = url.to_string();
    // Url::parse lowercases hosts; custom schemes need explicit host casing.
    if let Ok(parsed) = Url::parse(&out) {
        if parsed.scheme() == "searchos-local" {
            if let Some(host) = parsed.host_str() {
                let lower = host.to_ascii_lowercase();
                if lower != host {
                    out = out.replacen(
                        &format!("searchos-local://{host}"),
                        &format!("searchos-local://{lower}"),
                        1,
                    );
                }
            }
        }
    }
    out.trim_end_matches('?').to_string()
}

/// Independent-source host for evidence accounting.
///
/// Local documents are namespaced by workspace: every `searchos-local://`
/// source is *not* collapsed into one host. HTTP(S) sources use the authority.
pub fn source_host(raw: &str) -> String {
    let canonical = canonical_url(raw);
    if let Some((scheme, rest)) = canonical.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if scheme == "searchos-local" {
            return format!("{scheme}://{authority}");
        }
        return authority
            .split('@')
            .next_back()
            .unwrap_or(authority)
            .to_string();
    }
    canonical
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(&canonical)
        .to_string()
}

fn fallback(raw: &str) -> String {
    let s = raw.trim_end_matches('?').trim_end_matches('/');
    if s.is_empty() {
        "/".into()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalization_preserves_case_sensitive_paths() {
        assert_eq!(
            canonical_url("HTTPS://Example.COM/Docs/Rust?utm_source=x#Part"),
            "https://example.com/Docs/Rust"
        );
        assert_eq!(
            canonical_url("https://example.com/API/v1/Users?b=2&a=1&gclid=z"),
            "https://example.com/API/v1/Users?a=1&b=2"
        );
    }

    #[test]
    fn canonicalization_is_identical_across_paths() {
        let a = canonical_url("https://Example.com/docs/?utm_source=x#frag");
        let b = canonical_url("https://example.com/docs");
        assert_eq!(a, b);
    }

    #[test]
    fn local_uris_keep_workspace_and_case_sensitive_key() {
        let c = canonical_url("searchos-local://WS_A/Notes/Alpha?utm_source=x#frag");
        assert_eq!(c, "searchos-local://ws_a/Notes/Alpha");
        assert_eq!(source_host(&c), "searchos-local://ws_a");
        assert_ne!(
            source_host("searchos-local://ws_a/notes/a"),
            source_host("searchos-local://ws_b/notes/a")
        );
    }
}
