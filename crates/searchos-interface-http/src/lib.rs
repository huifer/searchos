//! HTTP gateway (RFC-012).
//!
//! Thin transport layer: authentication, DTO validation, application call,
//! response mapping (RFC-012 12.3). Business logic lives in the application
//! layer; Tavily DTOs never reach the core (RFC-012 12.7).

pub mod routes;
pub mod tavily;

pub use routes::{serve, AppState};
