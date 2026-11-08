//! External provider layer (RFC-009).
//!
//! - [`traits`] — SearchProvider / FetchProvider / EmbeddingProvider traits.
//! - [`error`] — provider error taxonomy (RFC-009 9.7).
//! - [`retry`] — bounded retry with exponential backoff; 429 honors
//!   Retry-After; 403 never retries unboundedly.
//! - [`circuit`] — circuit breaker per provider (RFC-014 14.4).
//! - [`registry`] — provider registry with health checks.
//! - [`tavily`], [`brave`] — MVP official native adapters (RFC-009 9.5:
//!   初期只需要实现 Tavily 和 Brave).

pub mod brave;
pub mod circuit;
pub mod embedding;
pub mod error;
pub mod registry;
pub mod retry;
pub mod tavily;
pub mod traits;

pub use brave::BraveProvider;
pub use circuit::CircuitBreaker;
pub use error::{ProviderError, ProviderErrorKind};
pub use registry::{ProviderRegistry, RegisteredProvider};
pub use retry::{retry_with_policy, RetryPolicy};
pub use tavily::TavilyProvider;
pub use traits::{
    FetchRequest, FetchResponse, ProviderResult, SearchParams, SearchProvider, SearchResultEntry,
};
