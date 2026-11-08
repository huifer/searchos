//! Content extraction: HTML → title / markdown-ish text (RFC-010 10.5).

use regex::Regex;
use scraper::{Html, Selector};
use std::sync::OnceLock;

fn strip_untrusted_blocks(html: &str) -> String {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        ["script", "style", "noscript", "template", "svg"]
            .iter()
            .map(|tag| Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}>")).unwrap())
            .collect()
    });
    let mut out = html.to_string();
    for re in res {
        out = re.replace_all(&out, "").into_owned();
    }
    out
}

/// Extracted page content.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractedContent {
    pub title: Option<String>,
    /// Markdown-ish main text.
    pub markdown: String,
    /// Plain text (for BM25 + snippets).
    pub text: String,
}

/// Extracts title and readable content from an HTML document.
/// 网页内容是 Untrusted Data (RFC-014 14.2): extraction strips scripts and
/// styles; output is data, never instructions.
pub fn extract_html(html: &str) -> ExtractedContent {
    // Remove script/style/noscript/template blocks before parsing so their
    // content never leaks into evidence text (RFC-014 14.2).
    let cleaned = strip_untrusted_blocks(html);
    let doc = Html::parse_document(&cleaned);

    let title = doc
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|t| t.text().collect::<String>().trim().to_string())
        .filter(|t| !t.is_empty());

    let mut markdown = String::new();
    let mut text = String::new();

    for el in doc.select(&Selector::parse("h1,h2,h3,h4,p,li,pre,blockquote").unwrap()) {
        let name = el.value().name();
        let content = el.text().collect::<String>().trim().to_string();
        if content.is_empty() {
            continue;
        }
        match name {
            "h1" => {
                markdown.push_str(&format!("\n# {content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
            "h2" => {
                markdown.push_str(&format!("\n## {content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
            "h3" | "h4" => {
                markdown.push_str(&format!("\n### {content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
            "li" => {
                markdown.push_str(&format!("- {content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
            "pre" => {
                markdown.push_str(&format!("\n```\n{content}\n```\n"));
                text.push_str(&content);
                text.push('\n');
            }
            "blockquote" => {
                markdown.push_str(&format!("\n> {content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
            _ => {
                markdown.push_str(&format!("\n{content}\n"));
                text.push_str(&content);
                text.push('\n');
            }
        }
    }

    let markdown = markdown.trim().to_string();
    let text = text.trim().to_string();
    // Untrusted evidence marker appended at the source layer by callers.
    ExtractedContent {
        title,
        markdown,
        text,
    }
}

/// Extracts from plain text (no HTML structure).
pub fn extract_text(raw: &str) -> ExtractedContent {
    ExtractedContent {
        title: None,
        markdown: raw.trim().to_string(),
        text: raw.trim().to_string(),
    }
}

/// Extracts a compact snippet around the query terms, max `max_chars`.
pub fn snippet_for(text: &str, query_terms: &[&str], max_chars: usize) -> String {
    let lower = text.to_lowercase();
    let hit = query_terms
        .iter()
        .filter_map(|t| lower.find(&t.to_lowercase()))
        .min()
        .unwrap_or(0);
    let start = hit.saturating_sub(max_chars / 4);
    let mut end = (start + max_chars).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut start_b = start;
    while start_b > 0 && !text.is_char_boundary(start_b) {
        start_b -= 1;
    }
    let snippet = text[start_b..end].trim().to_string();
    if snippet.is_empty() {
        text.chars().take(max_chars).collect()
    } else {
        snippet
    }
}

/// Untrusted-evidence wrapper (RFC-014 14.2): content delivered to agents is
/// labeled so it is never treated as instructions.
pub fn untrusted_marker(source_url: &str) -> String {
    format!("[UNTRUSTED_EVIDENCE source={source_url}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_structure() {
        let html = r#"<html><head><title>Tokio Tutorial</title></head>
        <body><script>alert('x')</script><style>.a{}</style>
        <h1>Async Rust</h1><p>Tokio is an async runtime.</p>
        <ul><li>fast</li><li>safe</li></ul></body></html>"#;
        let c = extract_html(html);
        assert_eq!(c.title.as_deref(), Some("Tokio Tutorial"));
        assert!(c.markdown.contains("# Async Rust"));
        assert!(c.markdown.contains("- fast"));
        assert!(!c.text.contains("alert"), "script content stripped");
    }

    #[test]
    fn plain_text_passthrough() {
        let c = extract_text("hello\nworld");
        assert_eq!(c.text, "hello\nworld");
        assert!(c.title.is_none());
    }

    #[test]
    fn snippet_centers_on_query() {
        let text = "a ".repeat(100) + "tokio runtime is fast " + &"b ".repeat(100);
        let s = snippet_for(&text, &["tokio"], 40);
        assert!(s.contains("tokio runtime"));
    }

    #[test]
    fn untrusted_marker_labels_source() {
        let m = untrusted_marker("https://evil.example.com");
        assert!(m.starts_with("[UNTRUSTED_EVIDENCE"));
    }
}
