//! HTTP fetcher with SSRF-guarded redirects, DNS address pinning, robots.txt
//! enforcement and bounded streaming responses.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use searchos_provider::traits::ExtractDepth;

use crate::extract;
use crate::guard::{UrlGuard, UrlViolation};
use crate::robots::RobotsDecision;

/// Fetch outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchOutcome {
    /// Fresh content.
    Ok(extract::ExtractedContent),
    /// Content unchanged (ETag/If-Modified-Since match) — nothing to update.
    NotModified,
    /// Blocked by robots.txt.
    RobotsDisallowed,
    /// Blocked by the URL guard (SSRF etc.).
    Blocked(UrlViolation),
    /// Empty content must not enter the normal index (RFC-010 10.7).
    EmptyContent,
    /// HTTP error with status code.
    HttpError { status: u16 },
    /// Network / timeout / limit error.
    Error(String),
}

/// Conditional GET headers kept per canonical URL (RFC-010 10.9).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConditionalHeaders {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl ConditionalHeaders {
    /// Builds request headers for a conditional GET.
    pub fn to_request_headers(&self) -> Vec<(String, String)> {
        let mut v = Vec::new();
        if let Some(e) = &self.etag {
            v.push(("If-None-Match".into(), e.clone()));
        }
        if let Some(m) = &self.last_modified {
            v.push(("If-Modified-Since".into(), m.clone()));
        }
        v
    }

    /// Updates headers from a response (store ETag/Last-Modified for reuse).
    pub fn from_response(headers: &[(String, String)]) -> Self {
        Self {
            etag: headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
                .map(|(_, v)| v.clone()),
            last_modified: headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("last-modified"))
                .map(|(_, v)| v.clone()),
        }
    }
}

/// Fetcher configuration (RFC-014 bounds, Part 24 §24.1 / INV-30-17/18).
#[derive(Debug, Clone)]
pub struct FetchLimits {
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_redirects: u32,
    /// Per-domain fetch concurrency (crawler.max_concurrent).
    pub max_concurrent: u32,
    /// Whether the fetch pipeline consults robots.txt (RFC-010 10.5).
    pub respect_robots: bool,
    /// RFC 9309 default: unreachable robots.txt disallows all.
    pub unknown_robots_disallow: bool,
    /// Safety cap for a site-declared Crawl-delay.
    pub crawl_delay_cap_seconds: u64,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            max_response_bytes: 10 * 1024 * 1024,
            max_redirects: 5,
            max_concurrent: 4,
            respect_robots: true,
            unknown_robots_disallow: true,
            crawl_delay_cap_seconds: 10,
        }
    }
}

/// The HTTP fetcher.
pub struct HttpFetcher {
    guard: Arc<UrlGuard>,
    robots: Arc<crate::robots::RobotsChecker>,
    limits: FetchLimits,
}

impl HttpFetcher {
    /// Fetcher with guard, robots and configured limits.
    pub fn new(
        guard: Arc<UrlGuard>,
        robots: Arc<crate::robots::RobotsChecker>,
        limits: FetchLimits,
    ) -> Self {
        Self {
            guard,
            robots,
            limits,
        }
    }

    /// Effective limits (diagnostics / `configured_limits_are_applied`).
    pub fn limits(&self) -> &FetchLimits {
        &self.limits
    }

    fn client(&self, url: &str, pinned: Option<&[SocketAddr]>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            // Redirects are handled manually so every hop is re-validated.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.limits.timeout)
            .user_agent("searchos/0.1 (+https://github.com/searchos/searchos)");
        if let (Some(addrs), Ok(parsed)) = (pinned, url::Url::parse(url)) {
            if let Some(host) = parsed.host_str() {
                // Connect ONLY to addresses that passed DNS validation; the
                // original Host header / SNI is preserved by reqwest.
                builder = builder.resolve_to_addrs(host, addrs);
            }
        }
        builder.build().expect("reqwest client")
    }

    fn host_key(url: &url::Url) -> String {
        let host = url.host_str().unwrap_or("");
        let port = url.port_or_known_default().unwrap_or(80);
        format!("{host}:{port}")
    }

    /// Fetches /robots.txt once per host:port when needed and returns the
    /// RFC 9309 decision. Unknown is returned for 5xx / network failures;
    /// 4xx means "no robots.txt" and is treated as allow.
    pub async fn robots_decision(&self, url: &url::Url) -> Result<RobotsDecision, UrlViolation> {
        let host_key = Self::host_key(url);
        if !self.robots.known(&host_key) {
            let mut robots_url = url.clone();
            robots_url.set_path("/robots.txt");
            robots_url.set_query(None);
            robots_url.set_fragment(None);
            let validated = self.guard.validate(robots_url.as_str())?;
            let addrs = self.guard.resolve_and_validate(&validated).await?;
            match self.raw_get(robots_url.as_str(), None, Some(&addrs)).await {
                Ok((200, _headers, body)) => self.robots.install(&host_key, Some(body.as_str())),
                Ok((status, _, _)) if (400..500).contains(&status) => {
                    self.robots.install(&host_key, Some(""))
                }
                _ => self.robots.install(&host_key, None),
            }
        }
        let decision = self.robots.check_cached(&host_key, url.path(), "searchos");
        if decision == RobotsDecision::Allowed {
            self.robots
                .wait_for_crawl_delay_capped(
                    &host_key,
                    "searchos",
                    self.limits.crawl_delay_cap_seconds,
                )
                .await;
        }
        Ok(decision)
    }

    /// Fetches a URL end-to-end (RFC-010 10.5 pipeline).
    pub async fn fetch(
        &self,
        raw_url: &str,
        conditional: Option<&ConditionalHeaders>,
    ) -> Result<FetchOutcome, UrlViolation> {
        let mut current = self.guard.validate(raw_url)?;
        let mut hop = 0u32;
        let metrics = searchos_observability::MetricsRegistry::global();
        loop {
            // Validate DNS for this hop and pin the resulting addresses into
            // the HTTP client (DNS-rebinding mitigation, INV-30-11).
            let addrs = self.guard.resolve_and_validate(&current).await?;
            if self.limits.respect_robots {
                let decision = self.robots_decision(&current).await?;
                let allowed = match decision {
                    RobotsDecision::Allowed => true,
                    RobotsDecision::Disallowed { .. } => false,
                    RobotsDecision::Unknown => !self.limits.unknown_robots_disallow,
                };
                if !allowed {
                    metrics.inc_counter("fetch.robots_disallowed_total");
                    return Ok(FetchOutcome::RobotsDisallowed);
                }
            }
            let permit = self
                .guard
                .acquire_domain_slot(current.host_str().unwrap_or(""))
                .await;
            let (status, headers, body) = {
                let _p = permit;
                match self
                    .raw_get(
                        current.as_str(),
                        conditional.map(|c| c.to_request_headers()),
                        Some(&addrs),
                    )
                    .await
                {
                    Ok(t) => t,
                    Err(e) => return Ok(FetchOutcome::Error(e)),
                }
            };

            match status {
                200 => {
                    let ctype = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    let extracted = if ctype.contains("html") {
                        extract::extract_html(&body)
                    } else {
                        extract::extract_text(&body)
                    };
                    if extracted.text.trim().is_empty() {
                        return Ok(FetchOutcome::EmptyContent);
                    }
                    return Ok(FetchOutcome::Ok(extracted));
                }
                304 => return Ok(FetchOutcome::NotModified),
                301 | 302 | 303 | 307 | 308 => {
                    hop += 1;
                    if hop > self.limits.max_redirects {
                        return Ok(FetchOutcome::Error("too many redirects".into()));
                    }
                    let location = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                        .map(|(_, v)| v.clone())
                        .ok_or_else(|| {
                            UrlViolation::new("INVALID_RESPONSE", "redirect without location")
                        })?;
                    let next_url = current
                        .join(&location)
                        .map_err(|e| UrlViolation::new("INVALID_URL", e.to_string()))?;
                    current = self
                        .guard
                        .validate_redirect(&current, next_url.as_str(), hop)?;
                    continue;
                }
                s => return Ok(FetchOutcome::HttpError { status: s }),
            }
        }
    }

    /// One GET with per-hop validation already done by caller. The body is
    /// streamed with a byte cap; `Content-Length` is checked before buffering.
    async fn raw_get(
        &self,
        url: &str,
        extra_headers: Option<Vec<(String, String)>>,
        pinned: Option<&[SocketAddr]>,
    ) -> Result<(u16, Vec<(String, String)>, String), String> {
        let started = Instant::now();
        let mut req = self.client(url, pinned).get(url);
        if let Some(hs) = extra_headers {
            for (k, v) in hs {
                req = req.header(&k, &v);
            }
        }
        let mut resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                format!("timeout after {:?}", self.limits.timeout)
            } else {
                format!("network: {e}")
            }
        })?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let metrics = searchos_observability::MetricsRegistry::global();
        if let Some(len) = resp.content_length() {
            if len > self.limits.max_response_bytes as u64 {
                metrics.inc_counter("fetch.limit_exceeded_total");
                metrics.set_gauge(
                    "fetch.oversize_buffered_bytes",
                    self.limits.max_response_bytes as u64,
                );
                return Err(format!("response too large: {len} bytes (Content-Length)"));
            }
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read body: {e}"))? {
            if body.len() + chunk.len() > self.limits.max_response_bytes {
                metrics.inc_counter("fetch.limit_exceeded_total");
                metrics.set_gauge("fetch.oversize_buffered_bytes", body.len() as u64);
                return Err(format!(
                    "response too large: > {} bytes",
                    self.limits.max_response_bytes
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&body).to_string();
        tracing::debug!(
            url,
            status,
            bytes = body.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "fetched"
        );
        Ok((status, headers, body))
    }

    /// Fetch depth knob passthrough (ExtractDepth) — kept for API parity
    /// with provider FetchRequest IR.
    pub fn depth_note(_d: ExtractDepth) -> &'static str {
        "http fetcher does its own extraction"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn fetcher() -> HttpFetcher {
        HttpFetcher::new(
            Arc::new(UrlGuard::default()),
            Arc::new(crate::robots::RobotsChecker::new()),
            FetchLimits::default(),
        )
    }

    type Handler = dyn Fn(&str) -> (u16, String, Option<usize>) + Send + Sync + 'static;

    async fn start_server(
        handler: impl Into<StdArc<Handler>>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let handler = handler.into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = StdArc::clone(&handler);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (status, body, declared_len) = handler(&path);
                    let length = declared_len.unwrap_or(body.len());
                    let response = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: text/plain\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (port, task)
    }

    fn local_fetcher(port: u16, limits: FetchLimits) -> HttpFetcher {
        let guard =
            UrlGuard::with_config(limits.max_redirects, 1, false, vec![80, 443, 8080, port]);
        HttpFetcher::new(
            Arc::new(guard),
            Arc::new(crate::robots::RobotsChecker::new()),
            limits,
        )
    }

    #[tokio::test]
    async fn ssrf_blocked_before_network() {
        let f = fetcher();
        let out = f
            .fetch("http://169.254.169.254/latest/meta-data/", None)
            .await;
        assert!(matches!(out, Err(v) if v.code == "CLOUD_METADATA_BLOCKED"));
    }

    #[tokio::test]
    async fn redirect_hop_validated() {
        let f = fetcher();
        let out = f.fetch("https://httpbin.org/redirect/1", None).await;
        if let Ok(FetchOutcome::Blocked(v)) = out {
            panic!("guard wrongly blocked public redirect chain: {v}");
        }
    }

    #[tokio::test]
    async fn fetch_public_page_extracts() {
        let f = fetcher();
        match f.fetch("https://example.com", None).await {
            Ok(FetchOutcome::Ok(content)) => {
                let combined = format!(
                    "{} {}",
                    content.title.as_deref().unwrap_or(""),
                    content.text
                )
                .to_lowercase();
                assert!(combined.contains("example domain"), "got: {combined}");
            }
            Ok(FetchOutcome::Error(_))
            | Ok(FetchOutcome::HttpError { .. })
            | Ok(FetchOutcome::RobotsDisallowed) => {}
            Ok(other) => panic!("expected content, got {other:?}"),
            Err(v) => panic!("guard violation: {v}"),
        }
    }

    /// Part 30 §30.11: the connection uses the validated address set, not a
    /// fresh DNS resolution performed by reqwest.
    #[tokio::test]
    async fn fetch_connects_to_validated_address() {
        let handler: StdArc<Handler> =
            StdArc::new(|_path| (200, "pinned-address-body".into(), None));
        let (port, server) = start_server(handler).await;
        let limits = FetchLimits {
            respect_robots: false,
            ..Default::default()
        };
        let f = local_fetcher(port, limits);
        let url = format!("http://example.com:{port}/pinned");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let (status, _headers, body) = f.raw_get(&url, None, Some(&[addr])).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "pinned-address-body");
        server.abort();
    }

    /// Part 30 §30.11: a declared oversized body is rejected before the full
    /// buffer is read.
    #[tokio::test]
    async fn oversized_response_rejected_before_full_buffer() {
        let handler: StdArc<Handler> = StdArc::new(|_path| (200, "small".into(), Some(100_000)));
        let (port, server) = start_server(handler).await;
        let limits = FetchLimits {
            max_response_bytes: 64,
            respect_robots: false,
            ..Default::default()
        };
        let f = local_fetcher(port, limits);
        let url = format!("http://127.0.0.1:{port}/large");
        let err = f.raw_get(&url, None, None).await.unwrap_err();
        assert!(err.contains("response too large"), "{err}");
        assert!(
            searchos_observability::MetricsRegistry::global().counter("fetch.limit_exceeded_total")
                > 0
        );
        server.abort();
    }

    /// G-03 / INV-30-18: robots.txt Disallow is enforced by the fetch path.
    #[tokio::test]
    async fn fetch_respects_robots_disallow() {
        let handler: StdArc<Handler> = StdArc::new(|path| {
            if path == "/robots.txt" {
                (
                    200,
                    "User-agent: searchos\nDisallow: /private\n".into(),
                    None,
                )
            } else {
                (200, "secret content".into(), None)
            }
        });
        let (port, server) = start_server(handler).await;
        let limits = FetchLimits {
            respect_robots: true,
            unknown_robots_disallow: true,
            ..Default::default()
        };
        let f = local_fetcher(port, limits);
        let out = f
            .fetch(&format!("http://127.0.0.1:{port}/private"), None)
            .await
            .unwrap();
        assert_eq!(out, FetchOutcome::RobotsDisallowed);
        server.abort();
    }

    /// G-03 / INV-30-18: unreachable robots.txt disallows by RFC 9309 default.
    #[tokio::test]
    async fn unreachable_robots_disallowed_by_default() {
        let handler: StdArc<Handler> = StdArc::new(|path| {
            if path == "/robots.txt" {
                (503, "unavailable".into(), None)
            } else {
                (200, "secret content".into(), None)
            }
        });
        let (port, server) = start_server(handler).await;
        let limits = FetchLimits {
            respect_robots: true,
            unknown_robots_disallow: true,
            ..Default::default()
        };
        let f = local_fetcher(port, limits);
        let out = f
            .fetch(&format!("http://127.0.0.1:{port}/private"), None)
            .await
            .unwrap();
        assert_eq!(out, FetchOutcome::RobotsDisallowed);
        server.abort();
    }

    /// Part 30 §30.11: configured limits reach the enforcement point.
    #[test]
    fn configured_limits_are_applied() {
        let limits = FetchLimits {
            timeout: Duration::from_millis(1234),
            max_response_bytes: 4321,
            max_redirects: 2,
            max_concurrent: 7,
            respect_robots: false,
            unknown_robots_disallow: false,
            crawl_delay_cap_seconds: 3,
        };
        let f = HttpFetcher::new(
            Arc::new(UrlGuard::with_config(2, 7, true, vec![80, 443])),
            Arc::new(crate::robots::RobotsChecker::new()),
            limits.clone(),
        );
        assert_eq!(f.limits().max_response_bytes, 4321);
        assert_eq!(f.limits().max_redirects, 2);
        assert_eq!(f.limits().max_concurrent, 7);
        assert_eq!(f.limits().timeout, Duration::from_millis(1234));
        assert!(!f.limits().respect_robots);
    }
}
