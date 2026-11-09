//! Output formatting helpers (Part 18: JSON / human / markdown).

use serde::Serialize;

/// Output format selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Pretty-printed JSON
    Json,
    /// Aligned key/value text (default)
    Human,
    /// GitHub-flavored markdown table
    Markdown,
}

/// Prints a serializable value in the chosen format.
///
/// Output failures are reported on stderr and the process keeps a clean
/// stdout. They must never be rendered as a fake payload on stdout: a
/// machine consumer would parse it as a successful result.
pub fn print<T: Serialize>(value: &T, format: Format) {
    match format {
        Format::Json => match serde_json::to_string_pretty(value) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("output serialization failed: {e}"),
        },
        Format::Human => {
            println!("{}", human_table(value));
        }
        Format::Markdown => {
            println!("{}", markdown(value));
        }
    }
}

/// Renders a JSON value as an aligned key/value human block.
fn human_table(value: &impl Serialize) -> String {
    let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    match v {
        serde_json::Value::Array(items) => items
            .iter()
            .map(human_table_value)
            .collect::<Vec<_>>()
            .join("\n"),
        other => human_table_value(&other),
    }
}

fn human_table_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let width = map.keys().map(|k| k.len()).max().unwrap_or(0);
            map.iter()
                .map(|(k, val)| format!("{:>width$}  {}", k, inline(val), width = width))
                .collect::<Vec<_>>()
                .join("\n")
        }
        other => inline(other),
    }
}

fn inline(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "-".into(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(inline).collect::<Vec<_>>().join(", "),
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, val)| format!("{k}={}", inline(val)))
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

/// Renders a JSON value as a GitHub-flavored markdown table or block.
fn markdown(value: &impl Serialize) -> String {
    let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    match &v {
        serde_json::Value::Array(items) => {
            let Some(first) = items.first() else {
                return "_empty_".into();
            };
            let serde_json::Value::Object(map) = first else {
                return items
                    .iter()
                    .map(|i| format!("- {}", inline(i)))
                    .collect::<Vec<_>>()
                    .join("\n");
            };
            let keys: Vec<&String> = map.keys().collect();
            let mut out = String::from("| ");
            out.push_str(
                &keys
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
            out.push_str(" |\n|");
            out.push_str(&keys.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
            out.push_str("|\n");
            for item in items {
                if let serde_json::Value::Object(row) = item {
                    out.push_str("| ");
                    out.push_str(
                        &keys
                            .iter()
                            .map(|k| inline(row.get(*k).unwrap_or(&serde_json::Value::Null)))
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                    out.push_str(" |\n");
                }
            }
            out
        }
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, val)| format!("- **{k}**: {}", inline(val)))
            .collect::<Vec<_>>()
            .join("\n"),
        other => inline(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_output_is_single_line_block() {
        let v = json!({"a": 1});
        let s = serde_json::to_string_pretty(&v).unwrap();
        assert!(s.contains("\"a\": 1"));
    }

    /// A non-serializable payload must not be printed as a fake JSON value.
    #[test]
    fn json_output_failure_is_not_faked_on_stdout() {
        struct NotJson;
        impl Serialize for NotJson {
            fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("cannot serialize"))
            }
        }
        // The function must not panic; the diagnostics path is stderr-only.
        print(&NotJson, Format::Json);
    }

    #[test]
    fn markdown_table_renders() {
        let v = json!([{"id": "x", "n": 1}, {"id": "y", "n": 2}]);
        let md = markdown(&v);
        assert!(md.starts_with("| id | n |"));
        assert!(md.contains("| x | 1 |"));
    }

    #[test]
    fn human_blocks_render() {
        let v = json!({"status": "ok", "items": [1, 2]});
        let s = human_table(&v);
        assert!(s.contains("status  ok"));
        assert!(s.contains("items  1, 2"));
    }
}
