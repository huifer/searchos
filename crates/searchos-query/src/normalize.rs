//! Query normalization (RFC-003 3.2).
//!
//! Deterministic: lower-case, collapse whitespace, strip punctuation noise,
//! NFC-ish simplification. Normalized text feeds fingerprints and the
//! canonical cache, so any change here is a breaking cache-key change.

/// Normalizes a raw query into canonical form.
pub fn normalize_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_ws = true; // trim leading
    for ch in raw.chars() {
        let c = ch.to_lowercase().next().unwrap_or(ch);
        if c.is_whitespace() {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
            continue;
        }
        last_ws = false;
        // Normalize full-width punctuation and quotes common in CJK input.
        match c {
            '“' | '”' | '«' | '»' | '"' => {}
            '‘' | '’' | '\'' => {}
            '，' => out.push(','),
            '。' | '．' => out.push('.'),
            '：' => out.push(':'),
            '（' => out.push('('),
            '）' => out.push(')'),
            '?' | '？' => out.push('?'),
            _ => out.push(c),
        }
    }
    let trimmed = out.trim_end().to_string();
    if trimmed == "?" {
        String::new()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_whitespace_and_case() {
        assert_eq!(
            normalize_query("  Rust   Tokio  VERSION "),
            "rust tokio version"
        );
    }

    #[test]
    fn cjk_punctuation_normalized() {
        assert_eq!(normalize_query("Rust，还是 Go？"), "rust,还是 go?");
    }

    #[test]
    fn quotes_stripped() {
        assert_eq!(normalize_query("\"tokio\" [2024]"), "tokio [2024]");
    }
}
