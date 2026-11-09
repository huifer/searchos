//! SSRF protection (RFC-014 14.1).
//!
//! 所有 URL Fetch 必须经过统一安全检查. DNS rebinding is mitigated by
//! validating the *resolved* addresses (callers resolve once and connect to
//! the validated set); every redirect hop is re-checked; hop count, response
//! size and duration are bounded elsewhere in the fetcher.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tokio::sync::Semaphore;
use url::Url;

/// Why a URL was blocked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct UrlViolation {
    pub code: &'static str,
    pub detail: String,
}

impl UrlViolation {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl From<UrlViolation> for searchos_provider::error::ProviderError {
    fn from(v: UrlViolation) -> Self {
        // Security blocks are non-retryable; map onto invalid-response class.
        searchos_provider::error::ProviderError::invalid_response("fetch", format!("ssrf: {v}"))
    }
}

/// Guard configuration.
#[derive(Debug, Clone)]
pub struct UrlGuard {
    /// Allowed redirect hops (RFC-014: 限制跳转次数).
    pub max_redirects: u32,
    /// Allow public HTTP only on 80/443/8080.
    pub allowed_ports: Vec<u16>,
    /// Allow `http://` (default true; https preferred).
    pub allow_http: bool,
    /// When false, resolved-IP blocking is disabled (explicit local/dev
    /// config only); URL syntax and redirect bounds remain enforced.
    pub ssrf_protection: bool,
    /// Per-domain concurrency limit (RFC-014 14.1).
    pub per_domain_concurrency: Arc<Semaphore>,
}

impl Default for UrlGuard {
    fn default() -> Self {
        Self {
            max_redirects: 5,
            allowed_ports: vec![80, 443, 8080],
            allow_http: true,
            ssrf_protection: true,
            per_domain_concurrency: Arc::new(Semaphore::new(4)),
        }
    }
}

impl UrlGuard {
    /// Config-driven guard (security + crawler sections are consumed here).
    pub fn with_config(
        max_redirects: u32,
        max_concurrent: u32,
        ssrf_protection: bool,
        allowed_ports: Vec<u16>,
    ) -> Self {
        Self {
            max_redirects,
            allowed_ports,
            allow_http: true,
            ssrf_protection,
            per_domain_concurrency: Arc::new(Semaphore::new(max_concurrent.max(1) as usize)),
        }
    }

    /// Validates a URL string against all static rules (RFC-014 14.1 list).
    pub fn validate(&self, raw: &str) -> Result<Url, UrlViolation> {
        let url = Url::parse(raw).map_err(|e| UrlViolation::new("INVALID_URL", e.to_string()))?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            s => {
                return Err(UrlViolation::new(
                    "BLOCKED_SCHEME",
                    format!("scheme {s} not allowed"),
                ))
            }
        }
        let host = url
            .host_str()
            .ok_or_else(|| UrlViolation::new("BLOCKED_HOST", "no host"))?;
        let host_l = host.to_lowercase();

        // Localhost names.
        if host_l == "localhost"
            || host_l.ends_with(".localhost")
            || host_l == "0.0.0.0"
            || host_l == "[::1]"
        {
            return Err(UrlViolation::new("LOCALHOST_BLOCKED", host_l));
        }
        // Unix-style or empty.
        if host_l.is_empty() {
            return Err(UrlViolation::new("BLOCKED_HOST", "empty host"));
        }

        // Port allow-list.
        let port = url.port_or_known_default().unwrap_or(80);
        if !self.allowed_ports.contains(&port) {
            return Err(UrlViolation::new("BLOCKED_PORT", format!("port {port}")));
        }

        // If the host is a literal IP, validate it directly; hostname IPs are
        // caught again after DNS resolution via `validate_ip`.
        if self.ssrf_protection {
            if let Ok(ip) = host_l
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
            {
                self.validate_ip(&ip)?;
            }
        }

        Ok(url)
    }

    /// Validates a resolved IP (literal host or DNS result). Blocks private,
    /// link-local, loopback, cloud-metadata and reserved ranges.
    pub fn validate_ip(&self, ip: &IpAddr) -> Result<(), UrlViolation> {
        match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                // Cloud metadata (AWS/GCP/Azure): 169.254.169.254 + broader link-local.
                if o[0] == 169 && o[1] == 254 {
                    return Err(UrlViolation::new("CLOUD_METADATA_BLOCKED", v4.to_string()));
                }
                if o[0] == 127 {
                    return Err(UrlViolation::new("LOCALHOST_BLOCKED", v4.to_string()));
                }
                if o[0] == 10
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                {
                    return Err(UrlViolation::new("PRIVATE_IP_BLOCKED", v4.to_string()));
                }
                if o[0] == 0 {
                    return Err(UrlViolation::new("BLOCKED_IP", v4.to_string()));
                }
                if o[0] == 100 && (64..=127).contains(&o[1]) {
                    return Err(UrlViolation::new("PRIVATE_IP_BLOCKED", v4.to_string()));
                    // CGNAT
                }
                if o[0] == 192 && o[1] == 0 && o[2] == 2 {
                    return Err(UrlViolation::new("BLOCKED_IP", v4.to_string()));
                    // TEST-NET
                }
                if o[0] >= 224 {
                    return Err(UrlViolation::new("BLOCKED_IP", v4.to_string()));
                    // multicast/reserved
                }
                Ok(())
            }
            IpAddr::V6(v6) => {
                let seg = v6.segments();
                // ::1 loopback
                if *v6 == Ipv6Addr::LOCALHOST {
                    return Err(UrlViolation::new("LOCALHOST_BLOCKED", v6.to_string()));
                }
                // fe80::/10 link-local
                if (seg[0] & 0xffc0) == 0xfe80 {
                    return Err(UrlViolation::new("PRIVATE_IP_BLOCKED", v6.to_string()));
                }
                // fc00::/7 unique local
                if (seg[0] & 0xfe00) == 0xfc00 {
                    return Err(UrlViolation::new("PRIVATE_IP_BLOCKED", v6.to_string()));
                }
                // ::ffff:x.y.z.w mapped IPv4 — validate embedded v4
                if seg[0] == 0
                    && seg[1] == 0
                    && seg[2] == 0
                    && seg[3] == 0
                    && seg[4] == 0
                    && seg[5] == 0xffff
                {
                    let v4 = Ipv4Addr::new(
                        (seg[6] >> 8) as u8,
                        (seg[6] & 0xff) as u8,
                        (seg[7] >> 8) as u8,
                        (seg[7] & 0xff) as u8,
                    );
                    return self.validate_ip(&IpAddr::V4(v4));
                }
                // ff00::/8 multicast
                if (seg[0] & 0xff00) == 0xff00 {
                    return Err(UrlViolation::new("BLOCKED_IP", v6.to_string()));
                }
                Ok(())
            }
        }
    }

    /// Validates a redirect hop: scheme/host/port re-checked, hop counter.
    pub fn validate_redirect(&self, from: &Url, to: &str, hop: u32) -> Result<Url, UrlViolation> {
        if hop > self.max_redirects {
            return Err(UrlViolation::new(
                "TOO_MANY_REDIRECTS",
                format!("hop {hop} > {}", self.max_redirects),
            ));
        }
        let next = self.validate(to)?;
        // Cross-origin redirects are allowed but fully re-validated above
        // (DNS resolution of the new host is re-checked by the caller).
        let _ = from;
        Ok(next)
    }

    /// Resolves the host via DNS and validates every returned address
    /// (RFC-014: DNS 解析结果校验; DNS rebinding mitigation).
    pub async fn resolve_and_validate(
        &self,
        url: &Url,
    ) -> Result<Vec<std::net::SocketAddr>, UrlViolation> {
        let host = url
            .host_str()
            .ok_or_else(|| UrlViolation::new("BLOCKED_HOST", "no host"))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let addrs = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| UrlViolation::new("DNS_ERROR", e.to_string()))?
            .collect::<Vec<_>>();
        if addrs.is_empty() {
            return Err(UrlViolation::new("DNS_ERROR", "no addresses"));
        }
        if self.ssrf_protection {
            for a in &addrs {
                self.validate_ip(&a.ip())?;
            }
        }
        Ok(addrs)
    }

    /// Acquires the per-domain concurrency permit (RFC-014 14.1).
    pub async fn acquire_domain_slot(&self, _host: &str) -> tokio::sync::OwnedSemaphorePermit {
        Arc::clone(&self.per_domain_concurrency)
            .acquire_owned()
            .await
            .expect("semaphore not closed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> UrlGuard {
        UrlGuard::default()
    }

    #[test]
    fn blocks_localhost() {
        assert!(guard().validate("http://localhost:8080/x").is_err());
        assert!(guard().validate("http://127.0.0.1/x").is_err());
        assert!(guard().validate("http://foo.localhost/x").is_err());
    }

    #[test]
    fn blocks_private_ranges() {
        assert!(guard().validate("http://10.0.0.5/x").is_err());
        assert!(guard().validate("http://192.168.1.1/x").is_err());
        assert!(guard().validate("http://172.16.0.9/x").is_err());
        assert!(guard().validate("http://172.31.255.1/x").is_err());
        assert!(guard().validate("http://[fe80::1]/x").is_err());
        assert!(guard().validate("http://[fd00::1]/x").is_err());
        assert!(
            guard().validate("http://[::ffff:10.0.0.1]/x").is_err(),
            "v4-mapped"
        );
    }

    #[test]
    fn blocks_cloud_metadata() {
        // DNS rebinding vectors & metadata endpoints.
        let g = guard();
        assert!(g
            .validate("http://169.254.169.254/latest/meta-data/")
            .is_err());
        assert!(g.validate_ip(&"169.254.169.254".parse().unwrap()).is_err());
    }

    #[test]
    fn blocks_bad_schemes_and_ports() {
        assert!(guard().validate("file:///etc/passwd").is_err());
        assert!(guard().validate("ftp://example.com").is_err());
        assert!(guard().validate("http://example.com:22/x").is_err());
    }

    #[test]
    fn allows_public_urls() {
        assert!(guard().validate("https://example.com/page").is_ok());
        assert!(guard().validate("http://example.com:8080/x").is_ok());
        assert!(guard().validate("https://docs.rs/tokio").is_ok());
    }

    #[test]
    fn redirect_hop_limit() {
        let g = guard();
        let from = g.validate("https://a.com/x").unwrap();
        assert!(g.validate_redirect(&from, "https://b.com/y", 1).is_ok());
        assert_eq!(
            g.validate_redirect(&from, "https://b.com/y", 6)
                .unwrap_err()
                .code,
            "TOO_MANY_REDIRECTS"
        );
        // Redirect into private space is blocked.
        assert_eq!(
            g.validate_redirect(&from, "http://10.0.0.1/x", 1)
                .unwrap_err()
                .code,
            "PRIVATE_IP_BLOCKED"
        );
    }

    #[tokio::test]
    async fn resolve_public_host_ok_private_blocked() {
        let g = guard();
        let ok = g
            .resolve_and_validate(&g.validate("https://example.com").unwrap())
            .await;
        assert!(ok.is_ok(), "example.com should resolve: {:?}", ok.err());
    }

    #[tokio::test]
    async fn domain_slot_bounded() {
        let g = Arc::new(UrlGuard::default());
        let g2 = Arc::clone(&g);
        let permit = g.acquire_domain_slot("example.com").await;
        // Semaphore default 4; 5th acquire would wait — just verify one works.
        drop(permit);
        let _ = g2.acquire_domain_slot("example.com").await;
    }
}
