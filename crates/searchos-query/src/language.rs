//! Language detection (RFC-003 3.2).
//!
//! Deterministic heuristic: script-ratio based. Good enough to feed
//! fingerprints and cache scoping; downstream providers receive the raw query.

/// Detects the dominant language of a query. Returns `en` fallback.
pub fn detect_language(text: &str) -> String {
    let total = text.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return "en".into();
    }
    let mut cjk = 0usize;
    let mut cyrillic = 0usize;
    for c in text.chars() {
        let cp = c as u32;
        if (0x4E00..=0x9FFF).contains(&cp) || (0x3400..=0x4DBF).contains(&cp) {
            cjk += 1;
        } else if (0x400..=0x4FF).contains(&cp) {
            cyrillic += 1;
        }
    }
    if cjk as f32 / total as f32 > 0.3 {
        "zh".into()
    } else if cyrillic as f32 / total as f32 > 0.3 {
        "ru".into()
    } else {
        "en".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cjk() {
        assert_eq!(detect_language("Rust 和 Go 哪个更适合 Web 开发"), "zh");
    }

    #[test]
    fn detects_english_default() {
        assert_eq!(detect_language("What is the latest Tokio version?"), "en");
    }

    #[test]
    fn mixed_mostly_latin_is_en() {
        assert_eq!(detect_language("tokio axum 中文 name"), "en");
    }
}
