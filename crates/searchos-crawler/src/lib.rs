//! Site crawler system (RFC-010).
//!
//! 不同网站可以使用不同的专属 Crawler Extension; 每个网站独立维护;
//! 单站点失败不能影响其他站点; 连续失败达到阈值进入 Quarantine.
//! 不提供无限制绕过反爬的能力.

pub mod extract_rules;
pub mod manifest;
pub mod registry;
pub mod status;

pub use extract_rules::{DeclarativeExtractor, ExtractRule, ExtractorsSpec};
pub use manifest::{CrawlerManifest, CrawlerType, MatchSpec, RuntimeSpec};
pub use registry::{CrawlerRegistry, SelectedCrawler};
pub use status::{CrawlerErrorKind, CrawlerStatus, FailureTracker};
