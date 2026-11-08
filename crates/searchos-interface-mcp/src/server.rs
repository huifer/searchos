//! MCP stdio server (RFC-012 12.8).
//!
//! JSON-RPC 2.0 over stdin/stdout. Tools expose explicit parameter schemas:
//! `search`, `extract`, `memory_search`, `cost_summary`, `search_trace`.

use std::sync::Arc;

use searchos_core::SearchOsRuntime;
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::request::{SearchMode, SearchRequest};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Tool catalogue with input schemas (RFC-012: 必须暴露明确的参数 Schema).
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "search",
            "description": "Search local knowledge and the web with a quality-gated, cost-aware path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query" },
                    "profile": { "type": "string", "enum": ["general","technical","latest","research","exact","offline"] },
                    "mode": { "type": "string", "enum": ["local_only","local_first","external_first","external_only","hybrid"] },
                    "workspace_id": { "type": "string" },
                    "max_external_queries": { "type": "integer", "minimum": 0 },
                    "max_total_cost": { "type": "number", "minimum": 0 }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "extract",
            "description": "Fetch and extract readable content from a URL (SSRF-guarded).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "workspace_id": { "type": "string" },
                    "persist": { "type": "boolean", "default": true }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "memory_search",
            "description": "Search persisted search memory with namespace isolation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "workspace_id": { "type": "string" },
                    "project": { "type": "string" },
                    "agent": { "type": "string" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "cost_summary",
            "description": "Aggregate cost ledger summary (estimates, not real bills).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "search_trace",
            "description": "Fetch the explainable decision trace for a search.",
            "inputSchema": {
                "type": "object",
                "properties": { "trace_id": { "type": "string" } },
                "required": ["trace_id"]
            }
        }),
    ]
}

/// Tool catalogue constant (name list).
pub const TOOLS: &[&str] = &[
    "search",
    "extract",
    "memory_search",
    "cost_summary",
    "search_trace",
];

/// Handles one JSON-RPC message; returns the response (None for
/// notifications such as `notifications/initialized`).
pub async fn handle_message(runtime: Arc<SearchOsRuntime>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    let result: Result<Value, Value> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "searchos", "version": searchos_core::VERSION }
        })),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(runtime, &params).await,
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        other => Err(json!({ "code": -32601, "message": format!("unknown method {other}") })),
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

async fn call_tool(runtime: Arc<SearchOsRuntime>, params: &Value) -> Result<Value, Value> {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "search" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_string();
            if query.trim().is_empty() {
                return Err(json!({ "code": -32602, "message": "query required" }));
            }
            let profile = args
                .get("profile")
                .and_then(|p| p.as_str())
                .map(ProfileId::new)
                .unwrap_or_else(ProfileId::general);
            let mode = match args.get("mode").and_then(|m| m.as_str()) {
                Some("local_only") => SearchMode::LocalOnly,
                Some("external_first") => SearchMode::ExternalFirst,
                Some("external_only") => SearchMode::ExternalOnly,
                Some("hybrid") => SearchMode::Hybrid,
                _ => SearchMode::LocalFirst,
            };
            let ws = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(WorkspaceId::new)
                .unwrap_or_else(|| WorkspaceId::new("default"));
            let mut req = SearchRequest::new(query, profile, mode, ws)
                .map_err(|e| json!({ "code": -32602, "message": e.to_string() }))?;
            if let Some(n) = args.get("max_external_queries").and_then(|n| n.as_u64()) {
                req.budget.max_external_queries = n as u32;
            }
            if let Some(c) = args.get("max_total_cost").and_then(|c| c.as_f64()) {
                req.budget.max_total_cost_micros = (c * 1_000_000.0).round() as i64;
            }
            let resp = runtime
                .application
                .search_service
                .search(req)
                .await
                .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?;
            serde_json::to_value(resp)
                .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?
        }
        "extract" => {
            let url = args
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            if url.trim().is_empty() {
                return Err(json!({ "code": -32602, "message": "url required" }));
            }
            let ws = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(WorkspaceId::new)
                .unwrap_or_else(|| WorkspaceId::new("default"));
            let persist = args
                .get("persist")
                .and_then(|p| p.as_bool())
                .unwrap_or(true);
            if persist {
                let doc = runtime
                    .application
                    .fetch_service
                    .fetch_and_persist(&url, &ws)
                    .await
                    .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?;
                serde_json::to_value(doc).unwrap_or(Value::Null)
            } else {
                let content = runtime
                    .application
                    .fetch_service
                    .fetch_preview(&url)
                    .await
                    .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?;
                json!({ "url": url, "title": content.title, "content": content.text })
            }
        }
        "memory_search" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_string();
            let reader = searchos_domain::memory::MemoryNamespace {
                workspace: args
                    .get("workspace_id")
                    .and_then(|w| w.as_str())
                    .map(WorkspaceId::new)
                    .unwrap_or_else(|| WorkspaceId::new("default")),
                project: args
                    .get("project")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string()),
                agent: args
                    .get("agent")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string()),
            };
            let top_k = args.get("top_k").and_then(|k| k.as_u64()).unwrap_or(5) as usize;
            let hits = runtime
                .application
                .memory_service
                .search(&query, &reader, top_k)
                .await
                .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?;
            json!({ "results": hits })
        }
        "cost_summary" => {
            let summary = runtime
                .application
                .cost_service
                .summary(&Default::default())
                .await
                .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?;
            json!({ "summary": summary, "is_estimate": true })
        }
        "search_trace" => {
            let trace_id = args
                .get("trace_id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let repo = searchos_storage_sqlite::repos::DecisionRepo::new(&runtime.sqlite);
            let trace = repo.get_trace(&trace_id).ok().flatten().ok_or_else(
                || json!({ "code": -32602, "message": format!("trace {trace_id} not found") }),
            )?;
            serde_json::to_value(trace).unwrap_or(Value::Null)
        }
        other => {
            return Err(json!({ "code": -32601, "message": format!("unknown tool {other}") }));
        }
    };

    // MCP tool result envelope: a content array with a text JSON payload.
    Ok(json!({
        "content": [ { "type": "text", "text": serde_json::to_string_pretty(&text).unwrap_or_default() } ],
        "isError": false
    }))
}

/// Runs the stdio JSON-RPC loop (one JSON object per line).
pub async fn run_stdio(runtime: Arc<SearchOsRuntime>) -> Result<(), String> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    while let Some(line) = reader.next_line().await.map_err(|e| e.to_string())? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } });
                write_line(&mut stdout, &err).await?;
                continue;
            }
        };
        if let Some(resp) = handle_message(Arc::clone(&runtime), &msg).await {
            write_line(&mut stdout, &resp).await?;
        }
    }
    Ok(())
}

async fn write_line(out: &mut tokio::io::Stdout, v: &Value) -> Result<(), String> {
    let s = serde_json::to_string(v).map_err(|e| e.to_string())?;
    out.write_all(s.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    out.write_all(b"\n").await.map_err(|e| e.to_string())?;
    out.flush().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn runtime() -> (Arc<SearchOsRuntime>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = searchos_core::SearchOsConfig::default();
        cfg.app.data_dir = dir.path().to_path_buf();
        let rt = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();
        (rt, dir)
    }

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let (rt, _d) = runtime().await;
        let init = handle_message(
            rt.clone(),
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "searchos");
        let list = handle_message(rt, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await
            .unwrap();
        let names: Vec<String> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            TOOLS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        // 必须暴露明确的参数 Schema
        assert!(list["result"]["tools"][0]["inputSchema"]["required"][0] == "query");
    }

    #[tokio::test]
    async fn notifications_have_no_response() {
        let (rt, _d) = runtime().await;
        assert!(handle_message(
            rt,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn unknown_method_errors() {
        let (rt, _d) = runtime().await;
        let resp = handle_message(rt, &json!({"jsonrpc":"2.0","id":9,"method":"nope"}))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn cost_summary_tool_works() {
        let (rt, _d) = runtime().await;
        let resp = handle_message(
            rt,
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cost_summary","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("summary"));
    }

    #[tokio::test]
    async fn search_tool_requires_query() {
        let (rt, _d) = runtime().await;
        let resp = handle_message(
            rt,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }
}
