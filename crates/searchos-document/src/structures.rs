//! RFC-049: deterministic structured extraction.
//!
//! An agent that only receives prose has to re-parse code samples, parameter
//! tables and lists itself — every consumer re-implements the same brittle
//! regexes. This module extracts those structures ONCE, deterministically,
//! from the markdown-ish text SearchOS already stores.
//!
//! What it is: a parser, not a guesser. Every block it reports can be traced
//! back to a line range in the source text.
//!
//! What it is NOT: it does not interpret the content, does not invent
//! fields, and does not use an LLM. A parameter table becomes records with
//! the table's OWN header names as keys — nothing more.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The kind of structure found in the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructureKind {
    CodeBlock,
    Table,
    List,
    Heading,
    Link,
}

impl StructureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CodeBlock => "code_block",
            Self::Table => "table",
            Self::List => "list",
            Self::Heading => "heading",
            Self::Link => "link",
        }
    }
}

/// One extracted structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructureBlock {
    pub kind: StructureKind,
    /// The raw text of the block (for a table: the original rows).
    pub content: String,
    /// 1-based line where the block starts.
    pub line: usize,
    /// Fenced code language, when the block declared one.
    pub language: Option<String>,
    /// Heading level (1..=6), for headings only.
    pub level: Option<u8>,
    /// Link target, for links only.
    pub url: Option<String>,
    /// Link label, for links only.
    pub label: Option<String>,
    /// Table header cells, for tables only.
    pub headers: Vec<String>,
    /// Table rows (excluding the header), for tables only.
    pub rows: Vec<Vec<String>>,
}

/// A table flattened into records: the table's OWN header names as keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRecord {
    pub headers: Vec<String>,
    pub rows: Vec<BTreeMap<String, String>>,
}

impl TableRecord {
    /// The records, one JSON object per row.
    pub fn to_records(&self) -> Vec<serde_json::Value> {
        self.rows
            .iter()
            .map(|row| {
                serde_json::Value::Object(
                    row.iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect(),
                )
            })
            .collect()
    }
}

/// Everything one extraction found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StructureExtraction {
    pub blocks: Vec<StructureBlock>,
    /// Tables flattened into header-keyed records.
    pub tables: Vec<TableRecord>,
}

impl StructureExtraction {
    pub fn blocks_of(&self, kind: StructureKind) -> Vec<&StructureBlock> {
        self.blocks.iter().filter(|b| b.kind == kind).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// Splits a markdown table row into cells, honouring `\|` escapes.
fn split_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                cells.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    cells.push(current.trim().to_string());
    cells
}

/// Whether a line is a markdown table separator (`|---|---|`).
fn is_separator(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            !cell.is_empty()
                && cell
                    .chars()
                    .all(|c| c == '-' || c == ':' || c.is_whitespace())
        })
}

/// Whether a line looks like a table row (has at least one `|`).
fn is_table_row(line: &str) -> bool {
    line.contains('|') && !line.trim().is_empty()
}

/// Makes header names unique so a record is a well-formed object.
fn unique_headers(headers: &[String]) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(headers.len());
    for header in headers {
        let base = if header.is_empty() {
            "column".to_string()
        } else {
            header.clone()
        };
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            out.push(base);
        } else {
            out.push(format!("{base}_{count}"));
        }
    }
    out
}

/// Extracts every structure from markdown-ish text.
///
/// The scan is line-based and deterministic: the same text always yields the
/// same blocks in the same order.
pub fn extract_structures(text: &str) -> StructureExtraction {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks: Vec<StructureBlock> = Vec::new();
    let mut tables: Vec<TableRecord> = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();

        // Fenced code block.
        if trimmed.starts_with("```") {
            let language = trimmed
                .strip_prefix("```")
                .map(|rest| rest.trim().to_string())
                .filter(|lang| !lang.is_empty());
            let start = index + 1;
            let mut body = String::new();
            let mut cursor = index + 1;
            let mut closed = false;
            while cursor < lines.len() {
                if lines[cursor].trim().starts_with("```") {
                    closed = true;
                    break;
                }
                body.push_str(lines[cursor]);
                body.push('\n');
                cursor += 1;
            }
            blocks.push(StructureBlock {
                kind: StructureKind::CodeBlock,
                content: body.trim_end().to_string(),
                line: start,
                language,
                level: None,
                url: None,
                label: None,
                headers: Vec::new(),
                rows: Vec::new(),
            });
            index = if closed { cursor + 1 } else { cursor };
            continue;
        }

        // Table: header + separator + rows.
        if is_table_row(line) && index + 1 < lines.len() && is_separator(lines[index + 1]) {
            let headers = unique_headers(&split_row(line));
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut cursor = index + 2;
            while cursor < lines.len() && is_table_row(lines[cursor]) {
                let cells = split_row(lines[cursor]);
                rows.push(cells);
                cursor += 1;
            }
            let records = rows
                .iter()
                .map(|row| {
                    let mut map = BTreeMap::new();
                    for (position, header) in headers.iter().enumerate() {
                        let value = row.get(position).cloned().unwrap_or_default();
                        map.insert(header.clone(), value);
                    }
                    map
                })
                .collect();
            tables.push(TableRecord {
                headers: headers.clone(),
                rows: records,
            });
            blocks.push(StructureBlock {
                kind: StructureKind::Table,
                content: lines[index..cursor].join("\n"),
                line: index + 1,
                language: None,
                level: None,
                url: None,
                label: None,
                headers,
                rows,
            });
            index = cursor;
            continue;
        }

        // Heading.
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count() as u8;
            if (1..=6).contains(&level) && trimmed.chars().nth(level as usize) == Some(' ') {
                let text = trimmed[(level as usize + 1)..].trim().to_string();
                blocks.push(StructureBlock {
                    kind: StructureKind::Heading,
                    content: text,
                    line: index + 1,
                    language: None,
                    level: Some(level),
                    url: None,
                    label: None,
                    headers: Vec::new(),
                    rows: Vec::new(),
                });
                index += 1;
                continue;
            }
        }

        // List: contiguous items of the same marker family.
        if is_list_item(trimmed) {
            let start = index + 1;
            let mut items = Vec::new();
            let mut cursor = index;
            while cursor < lines.len() {
                let candidate = lines[cursor].trim();
                if !is_list_item(candidate) {
                    break;
                }
                items.push(strip_list_marker(candidate));
                cursor += 1;
            }
            blocks.push(StructureBlock {
                kind: StructureKind::List,
                content: items.join("\n"),
                line: start,
                language: None,
                level: None,
                url: None,
                label: None,
                headers: Vec::new(),
                rows: Vec::new(),
            });
            index = cursor;
            continue;
        }

        // Inline link.
        if let Some((label, url)) = first_link(line) {
            blocks.push(StructureBlock {
                kind: StructureKind::Link,
                content: label.clone(),
                line: index + 1,
                language: None,
                level: None,
                url: Some(url),
                label: Some(label),
                headers: Vec::new(),
                rows: Vec::new(),
            });
        }
        index += 1;
    }

    StructureExtraction { blocks, tables }
}

fn is_list_item(trimmed: &str) -> bool {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && trimmed[digits..].starts_with(". ")
}

fn strip_list_marker(trimmed: &str) -> String {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return trimmed[2..].trim().to_string();
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    trimmed[digits + 2..].trim().to_string()
}

/// The first markdown link on a line: `[label](url)`.
fn first_link(line: &str) -> Option<(String, String)> {
    let start = line.find('[')?;
    let rest = &line[start + 1..];
    let label_end = rest.find(']')?;
    let label = &rest[..label_end];
    let after = &rest[label_end + 1..];
    let after = after.strip_prefix('(')?;
    let url_end = after.find(')')?;
    Some((
        label.trim().to_string(),
        after[..url_end].trim().to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"# Retrieval guide

The ranking stage uses BM25.

```rust
fn score() -> f32 { 1.0 }
```

## Parameters

| Name | Type | Description |
|------|------|-------------|
| top_k | int | How many results to keep |
| timeout_ms | int | Per-source deadline |

- First rule
- Second rule

See the [reference](https://example.test/reference) for details.
"#;

    #[test]
    fn extracts_every_structure_kind() {
        let extraction = extract_structures(DOC);
        assert_eq!(extraction.blocks_of(StructureKind::Heading).len(), 2);
        assert_eq!(extraction.blocks_of(StructureKind::CodeBlock).len(), 1);
        assert_eq!(extraction.blocks_of(StructureKind::Table).len(), 1);
        assert_eq!(extraction.blocks_of(StructureKind::List).len(), 1);
        assert_eq!(extraction.blocks_of(StructureKind::Link).len(), 1);
    }

    #[test]
    fn table_becomes_header_keyed_records() {
        let extraction = extract_structures(DOC);
        let table = &extraction.tables[0];
        assert_eq!(table.headers, vec!["Name", "Type", "Description"]);
        assert_eq!(table.rows.len(), 2);
        let records = table.to_records();
        assert_eq!(records[0]["Name"], serde_json::json!("top_k"));
        assert_eq!(records[0]["Type"], serde_json::json!("int"));
        assert_eq!(
            records[1]["Description"],
            serde_json::json!("Per-source deadline")
        );
    }

    #[test]
    fn code_block_keeps_language_and_body() {
        let extraction = extract_structures(DOC);
        let block = extraction.blocks_of(StructureKind::CodeBlock)[0];
        assert_eq!(block.language.as_deref(), Some("rust"));
        assert!(block.content.contains("fn score()"));
        assert_eq!(block.line, 5);
    }

    #[test]
    fn heading_levels_are_preserved() {
        let extraction = extract_structures(DOC);
        let headings = extraction.blocks_of(StructureKind::Heading);
        assert_eq!(headings[0].level, Some(1));
        assert_eq!(headings[0].content, "Retrieval guide");
        assert_eq!(headings[1].level, Some(2));
    }

    #[test]
    fn list_items_are_grouped_and_stripped() {
        let extraction = extract_structures(DOC);
        let list = extraction.blocks_of(StructureKind::List)[0];
        assert_eq!(list.content, "First rule\nSecond rule");
    }

    #[test]
    fn link_label_and_target_are_captured() {
        let extraction = extract_structures(DOC);
        let link = extraction.blocks_of(StructureKind::Link)[0];
        assert_eq!(link.label.as_deref(), Some("reference"));
        assert_eq!(link.url.as_deref(), Some("https://example.test/reference"));
    }

    #[test]
    fn plain_prose_yields_nothing() {
        let extraction = extract_structures("Just a sentence with no structure at all.");
        assert!(extraction.is_empty());
        assert!(extraction.tables.is_empty());
    }

    #[test]
    fn duplicate_headers_stay_unique() {
        let text = "| Name | Name |\n|---|---|\n| a | b |";
        let extraction = extract_structures(text);
        assert_eq!(extraction.tables[0].headers, vec!["Name", "Name_2"]);
        let records = extraction.tables[0].to_records();
        assert_eq!(records[0]["Name"], serde_json::json!("a"));
        assert_eq!(records[0]["Name_2"], serde_json::json!("b"));
    }

    #[test]
    fn escaped_pipes_stay_inside_the_cell() {
        let text = "| Name | Value |\n|---|---|\n| flag | a \\| b |";
        let extraction = extract_structures(text);
        let records = extraction.tables[0].to_records();
        assert_eq!(records[0]["Value"], serde_json::json!("a | b"));
    }

    #[test]
    fn extraction_is_deterministic() {
        let first = extract_structures(DOC);
        let second = extract_structures(DOC);
        assert_eq!(first, second);
    }
}
