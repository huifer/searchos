//! Declarative field extraction (RFC-011 11.2 Declarative extensions).

use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// One extraction rule (extractors.yaml).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractRule {
    /// Field name, e.g. `title`, `price`, `release_version`.
    pub field: String,
    /// CSS selector.
    pub selector: String,
    /// `text` or `attr:<name>`.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Optional regex capture group index applied to the value.
    #[serde(default)]
    pub regex_group: Option<usize>,
    #[serde(default)]
    pub regex: Option<String>,
}

fn default_mode() -> String {
    "text".into()
}

/// extractors.yaml spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractorsSpec {
    pub fields: Vec<ExtractRule>,
}

impl ExtractorsSpec {
    /// Parses from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, String> {
        serde_yaml::from_str(yaml).map_err(|e| format!("extractors yaml: {e}"))
    }
}

/// Extraction failure (SELECTOR_NOT_FOUND per RFC-010 10.6).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("selector not found for field '{field}'")]
pub struct SelectorNotFound {
    pub field: String,
}

/// Applies declarative rules to HTML.
pub struct DeclarativeExtractor {
    spec: ExtractorsSpec,
}

impl DeclarativeExtractor {
    /// New extractor for a spec.
    pub fn new(spec: ExtractorsSpec) -> Self {
        Self { spec }
    }

    /// Extracts all configured fields; missing fields are reported as
    /// SelectorNotFound (rule rot is a signal, not silent None).
    pub fn extract(&self, html: &str) -> Result<Vec<(String, String)>, SelectorNotFound> {
        let doc = Html::parse_document(html);
        let mut out = Vec::new();
        for rule in &self.spec.fields {
            let sel = selectors()
                .get(&rule.selector)
                .cloned()
                .unwrap_or_else(|| Selector::parse(&rule.selector).unwrap());
            let Some(el) = doc.select(&sel).next() else {
                return Err(SelectorNotFound {
                    field: rule.field.clone(),
                });
            };
            let mut value = match rule.mode.as_str() {
                "text" => el.text().collect::<String>().trim().to_string(),
                m if m.starts_with("attr:") => el.value().attr(&m[5..]).unwrap_or("").to_string(),
                _ => el.text().collect::<String>().trim().to_string(),
            };
            if let Some(re_str) = &rule.regex {
                if let Ok(re) = Regex::new(re_str) {
                    if let Some(caps) = re.captures(&value) {
                        let idx = rule.regex_group.unwrap_or(1);
                        if let Some(m) = caps.get(idx) {
                            value = m.as_str().to_string();
                        }
                    }
                }
            }
            out.push((rule.field.clone(), value));
        }
        Ok(out)
    }
}

fn selectors() -> &'static std::collections::HashMap<String, Selector> {
    static CACHE: OnceLock<std::collections::HashMap<String, Selector>> = OnceLock::new();
    CACHE.get_or_init(std::collections::HashMap::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<html><body>
        <h1 class="title">tokio 1.39</h1>
        <span class="version">v1.39.0</span>
        <a href="/releases/tag/tokio-1.39.0" class="link">link</a>
    </body></html>"#;

    fn spec() -> ExtractorsSpec {
        ExtractorsSpec::from_yaml(
            r#"
fields:
  - field: title
    selector: h1.title
  - field: version
    selector: span.version
    regex: '^v(.+)$'
    regex_group: 1
  - field: url
    selector: a.link
    mode: "attr:href"
"#,
        )
        .unwrap()
    }

    #[test]
    fn extracts_text_attr_and_regex() {
        let ex = DeclarativeExtractor::new(spec());
        let out = ex.extract(HTML).unwrap();
        let get = |f: &str| out.iter().find(|(k, _)| k == f).map(|(_, v)| v.clone());
        assert_eq!(get("title").as_deref(), Some("tokio 1.39"));
        assert_eq!(get("version").as_deref(), Some("1.39.0"));
        assert_eq!(get("url").as_deref(), Some("/releases/tag/tokio-1.39.0"));
    }

    #[test]
    fn missing_selector_is_reported() {
        let bad = ExtractorsSpec::from_yaml(
            r#"
fields:
  - field: missing
    selector: h9.nothere
"#,
        )
        .unwrap();
        let ex = DeclarativeExtractor::new(bad);
        let err = ex.extract(HTML).unwrap_err();
        assert_eq!(err.field, "missing");
    }
}
