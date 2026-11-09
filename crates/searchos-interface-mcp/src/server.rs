//! MCP stdio server (RFC-012 12.8, RFC-021 21.5).
//!
//! JSON-RPC 2.0 over stdin/stdout. Tools expose explicit parameter schemas:
//! `search`, `fetch`, `extract`, `memory_search`, `document_search`,
//! `cost_summary`, `trace_inspect`, `search_trace` (compat alias).

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
            "description": "Search local knowledge and the web with a quality-gated, cost-aware path. mode=local_only forbids ALL external calls and returns insufficient_evidence when local knowledge cannot pass the quality gate; local_first/hybrid/external_first/external_only may acquire external evidence. Read the quality.status and status fields before trusting results.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query" },
                    "profile": { "type": "string", "enum": ["general","technical","latest","research","exact","offline"] },
                    "mode": { "type": "string", "enum": ["local_only","local_first","external_first","external_only","hybrid"] },
                    "workspace_id": { "type": "string" },
                    "max_external_queries": { "type": "integer", "minimum": 0 },
                    "max_total_cost": { "type": "number", "minimum": 0 },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "request_id": { "type": "string", "description": "Client idempotency key; replaying it does not re-run or re-bill an external search." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "fetch",
            "description": "Fetch and extract readable content from a URL without persisting it (SSRF-guarded, no external search). Use extract to persist the page as reusable local knowledge.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "workspace_id": { "type": "string" }
                },
                "required": ["url"]
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
            "name": "ingest_text",
            "description": "Import pasted text into the local knowledge store (RFC-029). Returns 201-style ingestion metadata; the document becomes searchable after the outbox drain.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "source_key": { "type": "string" },
                    "title": { "type": "string" },
                    "content_type": { "type": "string" },
                    "language": { "type": "string" },
                    "workspace_id": { "type": "string" }
                },
                "required": ["content"]
            }
        }),
        json!({
            "name": "ingest_file",
            "description": "Import in-memory file content with a filename label (RFC-029). The filename never touches the server filesystem.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_name": { "type": "string" },
                    "content": { "type": "string" },
                    "source_key": { "type": "string" },
                    "title": { "type": "string" },
                    "content_type": { "type": "string" },
                    "language": { "type": "string" },
                    "workspace_id": { "type": "string" }
                },
                "required": ["file_name", "content"]
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
            "name": "document_search",
            "description": "Search persisted local documents by URL or title text with a bounded SQL lookup (no external calls).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
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
            "name": "trace_inspect",
            "description": "Fetch the explainable, staged decision trace for a search.",
            "inputSchema": {
                "type": "object",
                "properties": { "trace_id": { "type": "string" } },
                "required": ["trace_id"]
            }
        }),
        json!({
            "name": "search_trace",
            "description": "Compatibility alias for trace_inspect (RFC-021 21.5).",
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
    "fetch",
    "extract",
    "ingest_text",
    "ingest_file",
    "memory_search",
    "document_search",
    "cost_summary",
    "trace_inspect",
    "search_trace",
];

/// Handles one JSON-RPC message; returns the response (None for
/// notifications such as `notifications/initialized`).
pub async fn handle_message(runtime: Arc<SearchOsRuntime>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    let result: Result<Value, Value> = match method {
        "initialize" => {
            let protocol = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .filter(|v| !v.trim().is_empty())
                .unwrap_or("2024-11-05");
            Ok(json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "searchos", "version": searchos_core::VERSION }
            }))
        }
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(runtime, &params).await,
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        other => Err(rpc_error(
            -32601,
            "INTERNAL_ERROR",
            format!("unknown method {other}"),
        )),
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

/// JSON-RPC error with a shared uppercase transport code in `data.code`
/// (Part 30 INV-30-19).
fn rpc_error(code: i64, error_code: &str, message: impl ToString) -> Value {
    json!({
        "code": code,
        "message": message.to_string(),
        "data": { "code": error_code }
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
                return Err(rpc_error(-32602, "VALIDATION_FAILED", "query required"));
            }
            let profile = args
                .get("profile")
                .and_then(|p| p.as_str())
                .map(ProfileId::new)
                .unwrap_or_else(ProfileId::general);
            let mode = match args.get("mode").and_then(|m| m.as_str()) {
                None => SearchMode::LocalFirst,
                Some(value) => SearchMode::parse(value)
                    .map_err(|message| rpc_error(-32602, "VALIDATION_FAILED", message))?,
            };
            let ws = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(WorkspaceId::new)
                .unwrap_or_else(|| WorkspaceId::new("default"));
            let mut req = SearchRequest::new(query, profile, mode, ws)
                .map_err(|e| rpc_error(-32602, "VALIDATION_FAILED", e.to_string()))?;
            if let Some(n) = args.get("max_external_queries").and_then(|n| n.as_u64()) {
                req.budget.max_external_queries = n as u32;
            }
            if let Some(c) = args.get("max_total_cost").and_then(|c| c.as_f64()) {
                req.budget.max_total_cost_micros = (c * 1_000_000.0).round() as i64;
            }
            if let Some(rid) = args.get("request_id").and_then(|r| r.as_str()) {
                req = req.with_request_id(rid);
            }
            if let Some(max) = args.get("max_results").and_then(|m| m.as_u64()) {
                req = req.with_max_results(max as u32);
            }
            let mut resp = runtime
                .application
                .search_service
                .search(req)
                .await
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            if let Some(max) = args.get("max_results").and_then(|m| m.as_u64()) {
                resp.results
                    .truncate(searchos_domain::request::clamp_max_results(max as u32) as usize);
            }
            serde_json::to_value(resp)
                .map_err(|e| json!({ "code": -32603, "message": e.to_string() }))?
        }
        "fetch" => {
            // Non-persisting URL read (RFC-021 21.5). No external search.
            let url = args
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            if url.trim().is_empty() {
                return Err(rpc_error(-32602, "VALIDATION_FAILED", "url required"));
            }
            let content = runtime
                .application
                .fetch_service
                .fetch_preview(&url)
                .await
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            json!({ "url": url, "title": content.title, "content": content.text })
        }
        "document_search" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_string();
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as u32;
            let repo = searchos_storage_sqlite::repos::DocumentRepo::new(&runtime.sqlite);
            let workspace = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .unwrap_or("default");
            let docs = repo
                .search(&query, limit, Some(workspace))
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            json!({ "documents": serde_json::to_value(docs).unwrap_or_default() })
        }
        "extract" => {
            let url = args
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string();
            if url.trim().is_empty() {
                return Err(rpc_error(-32602, "VALIDATION_FAILED", "url required"));
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
                    .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
                serde_json::to_value(doc).unwrap_or(Value::Null)
            } else {
                let content = runtime
                    .application
                    .fetch_service
                    .fetch_preview(&url)
                    .await
                    .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
                json!({ "url": url, "title": content.title, "content": content.text })
            }
        }
        "ingest_text" => {
            let content = args
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if content.trim().is_empty() {
                return Err(rpc_error(-32602, "EMPTY_CONTENT", "content required"));
            }
            let ws = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(WorkspaceId::new)
                .unwrap_or_else(|| WorkspaceId::new("default"));
            let mut req = searchos_application::ingest_service::IngestRequest::new(ws);
            req.source_key = args
                .get("source_key")
                .and_then(|s| s.as_str())
                .map(String::from);
            req.title = args.get("title").and_then(|s| s.as_str()).map(String::from);
            req.content_type = args
                .get("content_type")
                .and_then(|s| s.as_str())
                .map(String::from);
            req.language = args
                .get("language")
                .and_then(|s| s.as_str())
                .map(String::from);
            let doc = runtime
                .application
                .ingest_service
                .ingest_text(req, &content)
                .await
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            serde_json::to_value(doc).unwrap_or(Value::Null)
        }
        "ingest_file" => {
            let content = args
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let file_name = args
                .get("file_name")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            if content.trim().is_empty() {
                return Err(rpc_error(-32602, "EMPTY_CONTENT", "content required"));
            }
            if file_name.trim().is_empty() {
                return Err(rpc_error(-32602, "VALIDATION_FAILED", "file_name required"));
            }
            let ws = args
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(WorkspaceId::new)
                .unwrap_or_else(|| WorkspaceId::new("default"));
            let mut req = searchos_application::ingest_service::IngestRequest::new(ws);
            req.source_key = args
                .get("source_key")
                .and_then(|s| s.as_str())
                .map(String::from);
            req.title = args.get("title").and_then(|s| s.as_str()).map(String::from);
            req.content_type = args
                .get("content_type")
                .and_then(|s| s.as_str())
                .map(String::from);
            req.language = args
                .get("language")
                .and_then(|s| s.as_str())
                .map(String::from);
            let doc = runtime
                .application
                .ingest_service
                .ingest_upload(req, content.as_bytes(), Some(&file_name))
                .await
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            serde_json::to_value(doc).unwrap_or(Value::Null)
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
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            json!({ "results": hits })
        }
        "cost_summary" => {
            let summary = runtime
                .application
                .cost_service
                .summary(&Default::default())
                .await
                .map_err(|e| rpc_error(-32603, e.class().code, e.to_string()))?;
            json!({ "summary": summary, "is_estimate": true })
        }
        "trace_inspect" | "search_trace" => {
            let trace_id = args
                .get("trace_id")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let repo = searchos_storage_sqlite::repos::DecisionRepo::new(&runtime.sqlite);
            let trace = repo.get_trace(&trace_id).ok().flatten().ok_or_else(|| {
                rpc_error(-32602, "NOT_FOUND", format!("trace {trace_id} not found"))
            })?;
            serde_json::to_value(trace).unwrap_or(Value::Null)
        }
        other => {
            return Err(rpc_error(
                -32601,
                "INTERNAL_ERROR",
                format!("unknown tool {other}"),
            ));
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
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": rpc_error(-32700, "INTERNAL_ERROR", format!("parse error: {e}")),
                });
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
        // RFC-021 names seven tools; `search_trace` is the single
        // documented compatibility alias, and RFC-029 adds two ingestion
        // tools (follow-up amendment to RFC-021).
        assert_eq!(names.len(), 10, "tool catalogue drifted: {names:?}");
        assert_eq!(
            names
                .iter()
                .filter(|n| n.as_str() != "search_trace"
                    && n.as_str() != "ingest_text"
                    && n.as_str() != "ingest_file")
                .count(),
            7,
            "RFC-021 tool count drifted"
        );
        // 必须暴露明确的参数 Schema
        assert!(list["result"]["tools"][0]["inputSchema"]["required"][0] == "query");
    }

    #[tokio::test]
    async fn ingest_text_tool_works() {
        let (rt, _d) = runtime().await;
        let resp = handle_message(
            rt.clone(),
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ingest_text","arguments":{"content":"quokka habitat mcp","source_key":"mcp/quokka"}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        assert!(value["document_id"].as_str().is_some());
        rt.process_outbox(10).await.unwrap();
    }

    #[tokio::test]
    async fn ingest_file_tool_works() {
        let (rt, _d) = runtime().await;
        let resp = handle_message(
            rt.clone(),
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ingest_file","arguments":{"file_name":"notes.txt","content":"mcp file body","source_key":"mcp/file"}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        assert!(value["document_id"].as_str().is_some());
        rt.process_outbox(10).await.unwrap();
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
    async fn rfc021_tool_catalogue_is_complete() {
        let (rt, _d) = runtime().await;
        let list = handle_message(rt, &json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}))
            .await
            .unwrap();
        let names: Vec<String> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for required in [
            "search",
            "fetch",
            "extract",
            "memory_search",
            "document_search",
            "cost_summary",
            "trace_inspect",
        ] {
            assert!(names.contains(&required.to_string()), "missing {required}");
        }
        let search_tool = &list["result"]["tools"][0];
        assert!(search_tool["description"]
            .as_str()
            .unwrap()
            .contains("local_only"));
    }

    #[tokio::test]
    async fn initialize_echoes_client_protocol_version() {
        let (rt, _d) = runtime().await;
        let init = handle_message(
            rt,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2025-06-18" }
            }),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn search_tool_rejects_unknown_mode_instead_of_downgrading() {
        let (rt, _d) = runtime().await;
        // A case/alias typo must never silently become an external-capable
        // mode: local_only is the only mode that forbids ALL external calls.
        for bogus in ["LOCAL_ONLY", "localOnly", "local-only "] {
            let resp = handle_message(
                rt.clone(),
                &json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "tools/call",
                    "params": {
                        "name": "search",
                        "arguments": { "query": "q", "mode": bogus }
                    }
                }),
            )
            .await
            .unwrap();
            assert_eq!(
                resp["error"]["code"], -32602,
                "mode {bogus:?} must be rejected: {resp}"
            );
        }
        // The documented kebab-case alias still works everywhere.
        let ok = handle_message(
            rt,
            &json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": { "query": "q", "mode": "local-only" }
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(ok["result"]["isError"], false);
    }

    #[tokio::test]
    async fn document_search_uses_bounded_sql_lookup() {
        let (rt, _d) = runtime().await;
        let mut req =
            searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new("default"));
        req.source_key = Some("notes/doc-search".into());
        req.title = Some("Doc Search Handbook".into());
        rt.application
            .ingest_service
            .ingest_text(req, "local document body")
            .await
            .unwrap();
        let _ = rt.process_outbox(50).await;

        let resp = handle_message(
            rt,
            &json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": {
                    "name": "document_search",
                    "arguments": { "query": "handbook", "limit": 5 }
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let payload: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(
            payload["documents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["canonical_url"]
                    .as_str()
                    .unwrap_or("")
                    .contains("searchos-local://default/notes/doc-search")),
            "title match via SQL lookup: {payload}"
        );
    }

    #[tokio::test]
    async fn search_tool_honours_max_results() {
        let (rt, _d) = runtime().await;
        // Seed two local documents so the cap is observable.
        let mut req =
            searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new("default"));
        req.source_key = Some("notes/max-one".into());
        rt.application
            .ingest_service
            .ingest_text(req, "maximum results doc one tokio")
            .await
            .unwrap();
        let mut req =
            searchos_application::ingest_service::IngestRequest::new(WorkspaceId::new("default"));
        req.source_key = Some("notes/max-two".into());
        rt.application
            .ingest_service
            .ingest_text(req, "maximum results doc two tokio")
            .await
            .unwrap();
        let _ = rt.process_outbox(50).await;

        let resp = handle_message(
            rt,
            &json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "maximum results tokio",
                        "mode": "local_only",
                        "max_results": 1
                    }
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let payload: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            payload["results"].as_array().unwrap().len(),
            1,
            "max_results must trim the response: {payload}"
        );
    }

    #[tokio::test]
    async fn document_search_and_trace_inspect_tools_work() {
        let (rt, _d) = runtime().await;
        let docs = handle_message(
            rt.clone(),
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"document_search","arguments":{"query":"example.com"}}}),
        )
        .await
        .unwrap();
        assert_eq!(docs["result"]["isError"], false);
        assert!(docs["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("documents"));
        let missing = handle_message(
            rt,
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"trace_inspect","arguments":{"trace_id":"nope"}}}),
        )
        .await
        .unwrap();
        assert_eq!(missing["error"]["code"], -32602);
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
