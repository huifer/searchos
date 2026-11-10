//! MCP transport gate (ADR-031).
//!
//! SearchOS v1 speaks MCP **over stdio only**: no HTTP listener, no streamable
//! HTTP, no MCP route on the gateway. These assertions keep the decision from
//! decaying into "the docs claim something the code does not do" — the failure
//! mode this gate exists to prevent.
//!
//! See `docs/mcp.md` for the contract and the rationale.

use searchos_interface_http::routes::{api_route_table, dashboard_route_table};
use searchos_interface_mcp::TRANSPORT;

#[test]
fn mcp_transport_is_stdio() {
    assert_eq!(
        TRANSPORT, "stdio",
        "ADR-031 freezes the v1 MCP transport to stdio"
    );
}

/// The MCP crate must not grow an HTTP server. If remote MCP is ever added,
/// this fails and forces the ADR + `docs/mcp.md` + the gateway contract to be
/// updated together.
#[test]
fn mcp_crate_has_no_http_server_dependency() {
    const MANIFEST: &str = include_str!("../../../crates/searchos-interface-mcp/Cargo.toml");
    for forbidden in [
        "axum",
        "hyper",
        "tower-http",
        "actix-web",
        "warp",
        "poem",
        "salvo",
        "socketioxide",
        "tokio-tungstenite",
    ] {
        assert!(
            !MANIFEST.contains(forbidden),
            "crates/searchos-interface-mcp must stay stdio-only (ADR-031); found '{forbidden}'"
        );
    }
    assert!(
        MANIFEST.contains("searchos-observability"),
        "the cancellation acknowledgement must be counted"
    );
}

/// The gateway must not expose an MCP route, on either the operational or the
/// dashboard surface.
#[test]
fn gateway_exposes_no_mcp_route() {
    let routes: Vec<String> = api_route_table()
        .into_iter()
        .chain(dashboard_route_table())
        .map(|spec| format!("{} {}", spec.method, spec.path))
        .collect();
    let mcp_routes: Vec<&String> = routes
        .iter()
        .filter(|route| route.to_lowercase().contains("mcp"))
        .collect();
    assert!(
        mcp_routes.is_empty(),
        "the gateway must not serve MCP (ADR-031); found {mcp_routes:?}"
    );
    // Sanity: the table is the real one.
    assert!(routes.iter().any(|route| route.contains("/search")));
    assert!(routes
        .iter()
        .any(|route| route.contains("/api/v1/dashboard/")));
}

/// The normative docs must describe stdio-only and must not advertise a remote
/// MCP transport as a current capability.
#[test]
fn docs_do_not_claim_a_remote_mcp_transport() {
    const ARCHITECTURE: &str = include_str!("../../../docs/architecture.md");
    const README_EN: &str = include_str!("../../../README.md");
    const README_ZH: &str = include_str!("../../../README.zh.md");
    const MCP_DOC: &str = include_str!("../../../docs/mcp.md");

    // The stale claims that started this decision.
    assert!(
        !ARCHITECTURE.contains("HTTP API / remote MCP"),
        "docs/architecture.md still lists remote MCP as a Gateway entry"
    );
    assert!(
        !README_EN.contains("REST + MCP"),
        "README.md still advertises MCP on the HTTP gateway"
    );
    assert!(
        !README_ZH.contains("REST + MCP"),
        "README.zh.md still advertises MCP on the HTTP gateway"
    );

    // The corrected statements must be present, in both languages.
    assert!(
        ARCHITECTURE.contains("ADR-031"),
        "docs/architecture.md must record the stdio-only decision"
    );
    assert!(
        README_EN.contains("stdio-only"),
        "README.md must state that MCP is stdio-only"
    );
    assert!(
        README_ZH.contains("仅支持 **stdio**"),
        "README.zh.md must state that MCP is stdio-only"
    );
    assert!(
        MCP_DOC.contains("ADR-031"),
        "docs/mcp.md must record the decision"
    );
    assert!(
        MCP_DOC.contains("notifications/cancelled"),
        "docs/mcp.md must document the cancellation behaviour"
    );
}

/// The stdio tool surface is the documented one, including the compatibility
/// alias; a silent rename or removal fails here.
#[test]
fn mcp_tool_catalogue_is_the_documented_one() {
    let names: Vec<&str> = searchos_interface_mcp::TOOLS.to_vec();
    for expected in [
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
    ] {
        assert!(
            names.contains(&expected),
            "MCP tool '{expected}' is missing from the catalogue: {names:?}"
        );
    }

    // Every advertised tool carries an explicit parameter schema and a
    // description (RFC-012 12.8), and the name list matches the schema list.
    let advertised = searchos_interface_mcp::server::tools();
    assert_eq!(
        advertised.len(),
        names.len(),
        "TOOLS and tools() must describe the same catalogue"
    );
    for tool in &advertised {
        let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        assert!(
            names.contains(&name),
            "tool '{name}' is advertised but missing from TOOLS"
        );
        assert!(
            tool.get("inputSchema").is_some(),
            "MCP tool '{name}' has no inputSchema"
        );
        assert!(
            tool.get("description")
                .and_then(|d| d.as_str())
                .is_some_and(|d| !d.trim().is_empty()),
            "MCP tool '{name}' has no description"
        );
    }
}

/// A cancellation notification produces no response but is acknowledged
/// (counted + logged) instead of being silently dropped.
#[tokio::test]
async fn cancellation_is_acknowledged_not_silently_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = searchos_core::SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let runtime = searchos_core::SearchOsRuntime::init(cfg).await.unwrap();

    let before = searchos_observability::MetricsRegistry::global()
        .counter("mcp.cancellation_unsupported_total");
    let response = searchos_interface_mcp::handle_message(
        runtime,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 1, "reason": "user" }
        }),
    )
    .await;
    assert!(
        response.is_none(),
        "a JSON-RPC notification must not produce a response"
    );
    let after = searchos_observability::MetricsRegistry::global()
        .counter("mcp.cancellation_unsupported_total");
    assert!(
        after > before,
        "cancellation must be counted, not silently dropped"
    );
}
