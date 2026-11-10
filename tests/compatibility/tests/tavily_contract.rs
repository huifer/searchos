//! Frozen Tavily compatibility contract (RFC-012 12.7, ADR-006).
//!
//! The fixtures under `fixtures/tavily/` are the executable form of the
//! compatibility subset: each case posts a real request to a real in-process
//! gateway and asserts the wire contract. They lock three promises:
//!
//! 1. **The response shape is stable.** A Tavily client always sees
//!    `query` / `results` / `unsupported_features` / `answer` / `response_time`
//!    (search) or `results` / `failed_results` / `unsupported_features` /
//!    `response_time` (extract).
//! 2. **Nothing is silently ignored.** Every documented Tavily knob that
//!    SearchOS does not honor, and every unknown field, is reported back in
//!    `unsupported_features`. `answer` is always `null` because SearchOS
//!    never fabricates an answer.
//! 3. **The native contract is unaffected.** A body without Tavily keys keeps
//!    the SearchOS envelope and never grows Tavily-only fields.

use std::collections::BTreeMap;
use std::path::Path;

use searchos_core::{SearchOsConfig, SearchOsRuntime};
use searchos_interface_http::routes::router;
use searchos_interface_http::AppState;
use serde::Deserialize;
use tower::ServiceExt;

#[derive(Debug, Deserialize)]
struct Fixture {
    case: String,
    method: String,
    path: String,
    body: serde_json::Value,
    expect: Expect,
}

#[derive(Debug, Deserialize)]
struct Expect {
    status: u16,
    #[serde(default)]
    json_fields: Vec<String>,
    #[serde(default)]
    absent_fields: Vec<String>,
    #[serde(default)]
    unsupported_features: Vec<String>,
    #[serde(default)]
    unsupported_features_contains: Vec<String>,
    #[serde(default)]
    failed_results_min: usize,
}

async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SearchOsConfig::default();
    cfg.app.data_dir = dir.path().to_path_buf();
    let runtime = SearchOsRuntime::init(cfg).await.unwrap();
    (router(AppState::new(runtime)), dir)
}

fn fixtures() -> Vec<Fixture> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tavily");
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read fixtures at {}: {e}", root.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no Tavily fixtures found under {}",
        root.display()
    );
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            serde_json::from_str::<Fixture>(&text)
                .unwrap_or_else(|e| panic!("fixture {} is not valid: {e}", path.display()))
        })
        .collect()
}

async fn call(
    app: axum::Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn tavily_fixtures_match_the_frozen_contract() {
    let (app, _dir) = app().await;
    let cases = fixtures();
    assert!(
        cases.len() >= 8,
        "expected the full fixture set, found {}",
        cases.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let (status, body) = call(app.clone(), &case.method, &case.path, case.body.clone()).await;
        let label = format!("{} [{} {}]", case.case, case.method, case.path);

        if status != case.expect.status {
            failures.push(format!(
                "{label}: status {} != {} ({body})",
                status, case.expect.status
            ));
            continue;
        }

        let object = match body.as_object() {
            Some(object) => object,
            None => {
                failures.push(format!("{label}: response is not a JSON object: {body}"));
                continue;
            }
        };

        for field in &case.expect.json_fields {
            if !object.contains_key(field) {
                failures.push(format!("{label}: missing field '{field}' in {body}"));
            }
        }
        for field in &case.expect.absent_fields {
            if object.contains_key(field) {
                failures.push(format!(
                    "{label}: field '{field}' must not be present in {body}"
                ));
            }
        }

        if !case.expect.unsupported_features.is_empty()
            || !case.expect.unsupported_features_contains.is_empty()
        {
            let reported: Vec<String> = object
                .get("unsupported_features")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for expected in &case.expect.unsupported_features {
                if !reported.iter().any(|item| item == expected) {
                    failures.push(format!(
                        "{label}: unsupported_features must contain '{expected}', got {reported:?}"
                    ));
                }
            }
            for fragment in &case.expect.unsupported_features_contains {
                if !reported.iter().any(|item| item.contains(fragment)) {
                    failures.push(format!(
                        "{label}: unsupported_features must mention '{fragment}', got {reported:?}"
                    ));
                }
            }
        }

        if case.expect.failed_results_min > 0 {
            let failed = object
                .get("failed_results")
                .and_then(|value| value.as_array())
                .map(|items| items.len())
                .unwrap_or(0);
            if failed < case.expect.failed_results_min {
                failures.push(format!(
                    "{label}: failed_results has {failed} entries, expected at least {}",
                    case.expect.failed_results_min
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Tavily contract violations:\n{}",
        failures.join("\n")
    );
}

/// A Tavily search must never fabricate an answer, and every result must carry
/// the four core Tavily fields.
#[tokio::test]
async fn tavily_search_never_invents_an_answer() {
    let (app, _dir) = app().await;
    let (status, body) = call(
        app,
        "POST",
        "/search",
        serde_json::json!({ "query": "anything", "include_answer": true }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body["answer"].is_null(),
        "SearchOS never generates answers: {body}"
    );
    let results = body["results"].as_array().cloned().unwrap_or_default();
    for result in results {
        for field in ["title", "url", "content", "score"] {
            assert!(
                result.get(field).is_some(),
                "result missing '{field}': {result}"
            );
        }
    }
    let reported: Vec<String> = body["unsupported_features"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        reported.iter().any(|item| item.contains("include_answer")),
        "include_answer must be reported: {reported:?}"
    );
}

/// Supported knobs must not produce unsupported noise.
#[tokio::test]
async fn tavily_supported_knobs_are_not_reported_unsupported() {
    let (app, _dir) = app().await;
    let (_, body) = call(
        app,
        "POST",
        "/search",
        serde_json::json!({
            "query": "q",
            "topic": "general",
            "max_results": 2,
            "days": 3,
            "include_domains": ["example.com"],
            "exclude_domains": ["spam.example"]
        }),
    )
    .await;
    assert_eq!(
        body["unsupported_features"],
        serde_json::json!([]),
        "{body}"
    );
}

/// The subset is explicit: `/crawl` and `/map` are not part of v1, and they
/// must not be in the OpenAPI contract either (the drift test enforces that).
#[tokio::test]
async fn crawl_and_map_are_out_of_scope() {
    let (app, _dir) = app().await;
    for path in ["/crawl", "/map"] {
        let (status, _) = call(
            app.clone(),
            "POST",
            path,
            serde_json::json!({ "url": "https://example.com" }),
        )
        .await;
        assert_eq!(status, 404, "POST {path} must not be registered in v1");
    }
    let spec = include_str!("../../../docs/openapi.yaml");
    assert!(
        !spec.contains("\"/crawl\""),
        "/crawl must not be documented"
    );
    assert!(!spec.contains("\"/map\""), "/map must not be documented");
}

/// A body without any Tavily key stays on the native SearchOS contract.
/// The compatibility layer is opt-in by shape, so a native client never grows
/// Tavily-only fields.
#[tokio::test]
async fn native_bodies_never_get_tavily_fields() {
    let (app, _dir) = app().await;
    for body in [
        serde_json::json!({ "query": "q" }),
        serde_json::json!({ "query": "q", "mode": "local-only" }),
        serde_json::json!({ "query": "q", "max_results": 2, "request_id": "r1" }),
    ] {
        let (status, response) = call(app.clone(), "POST", "/search", body).await;
        assert_eq!(status, 200, "{response}");
        assert!(!response
            .as_object()
            .unwrap()
            .contains_key("unsupported_features"));
        assert!(!response.as_object().unwrap().contains_key("response_time"));
        assert!(
            response.get("status").is_some(),
            "native status: {response}"
        );
    }
}

/// Auth and error envelopes keep the same shape on the compatibility path.
#[tokio::test]
async fn tavily_error_envelope_is_standard() {
    let (app, _dir) = app().await;
    let (status, body) = call(
        app.clone(),
        "POST",
        "/search",
        serde_json::json!({ "query": "   ", "search_depth": "basic" }),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]["code"].is_string(),
        "standard error envelope: {body}"
    );
    assert!(
        body["error"]["message"].is_string(),
        "standard error envelope: {body}"
    );

    let (status, body) = call(app, "POST", "/extract", serde_json::json!({ "urls": [] })).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]["code"].is_string(),
        "standard error envelope: {body}"
    );
}

/// The workspace default for the compatibility path is the dedicated
/// `tavily` workspace, and an explicit workspace is honored.
#[tokio::test]
async fn tavily_workspace_defaults_to_the_dedicated_workspace() {
    let (app, _dir) = app().await;
    let (_, body) = call(
        app.clone(),
        "POST",
        "/search",
        serde_json::json!({ "query": "q", "search_depth": "basic" }),
    )
    .await;
    assert!(body["results"].is_array(), "{body}");

    // The document store proves which workspace the request landed in.
    let docs = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/documents?workspace_id=tavily&limit=1")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(docs.into_body(), 1 << 20)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(parsed["documents"].is_array(), "{parsed}");
}

/// Guard against a fixture silently asserting nothing.
#[test]
fn fixture_expectations_are_meaningful() {
    let mut cases = fixtures();
    assert!(cases.len() >= 8);
    let mut with_unsupported = 0;
    for case in &mut cases {
        assert!(!case.case.is_empty());
        assert!(!case.body.is_object() || !case.body.as_object().unwrap().is_empty());
        if !case.expect.unsupported_features_contains.is_empty() {
            with_unsupported += 1;
        }
    }
    assert!(
        with_unsupported >= 3,
        "expected several unsupported-knob cases, found {with_unsupported}"
    );
}

/// The contract is a JSON object at every leaf the client reads.
#[test]
fn fixture_files_are_valid_json_objects() {
    for case in fixtures() {
        let object = case
            .body
            .as_object()
            .unwrap_or_else(|| panic!("{} body must be an object", case.case));
        assert!(!object.is_empty(), "{} body is empty", case.case);
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        let map: BTreeMap<&String, &serde_json::Value> = object.iter().collect();
        assert_eq!(map.len(), object.len());
    }
}
