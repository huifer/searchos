//! `searchos` CLI — Local Mode entry point (Part 18 command contract).
//!
//! Hard constraint (P-003): the CLI calls the in-process application. It
//! never round-trips through localhost HTTP; only `gateway start` binds a
//! socket (that is the Gateway Mode server itself).

mod output;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use output::{print, Format};
use searchos_application::ingest_service::{IngestBatchItem, IngestPayload, IngestRequest};
use searchos_core::{SearchOsConfig, SearchOsRuntime};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::{MemoryNamespace, SharingPolicy};
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::{SearchOsError, ValidationError};
use searchos_evaluation::{runner::QueryResult as EvalQueryResult, EvaluationDataset};
use searchos_storage_sqlite::repos;

/// SearchOS — quality-first, cost-aware search infrastructure for AI agents.
///
/// The CLI runs in Local Mode: it calls the in-process runtime directly and
/// never round-trips through localhost HTTP. `gateway start` is the exception
/// — it runs the Gateway Mode HTTP server itself. Data lives in ~/.searchos
/// unless --data-dir or SEARCHOS_DATA_DIR overrides it.
#[derive(Parser)]
#[command(
    name = "searchos",
    version,
    about = "Quality-first, cost-aware search infrastructure for AI agents",
    long_about = "SearchOS — quality-first, cost-aware search infrastructure for AI agents.\n\n\
                  Local Mode is in-process: the CLI calls the runtime directly and never talks to a\n\
                  local HTTP server. `gateway start` is the exception — it runs the Gateway Mode HTTP\n\
                  server itself. Data lives in ~/.searchos unless --data-dir or SEARCHOS_DATA_DIR\n\
                  overrides it. Every command accepts --format json|human|markdown."
)]
struct Cli {
    /// Config file override (env: SEARCHOS_CONFIG)
    #[arg(long, global = true, env = "SEARCHOS_CONFIG")]
    config: Option<PathBuf>,
    /// Data directory override (env: SEARCHOS_DATA_DIR)
    #[arg(long, global = true, env = "SEARCHOS_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Output format for command results
    #[arg(long, global = true, value_enum, default_value = "human")]
    format: Format,
    /// Non-interactive (never prompt)
    #[arg(long, global = true, default_value_t = true)]
    non_interactive: bool,
    /// Increase log verbosity: -v = info, -vv = debug, -vvv = trace (RUST_LOG overrides)
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize the data directory and database
    Init,
    /// Diagnose configuration and storage health
    Doctor,
    /// Print version information
    Version,
    /// Generate shell completion scripts
    Completions {
        /// Target shell: bash | zsh | fish | powershell | elvish
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Execute a search (quality-gated, cost-aware)
    Search {
        /// Search query text
        query: String,
        /// Search profile: general | fast | deep | latest
        #[arg(long, default_value = "general")]
        profile: String,
        /// local_only | local_first | external_first | external_only | hybrid
        #[arg(long, default_value = "local_first")]
        mode: String,
        /// Workspace that owns the search and its memory
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Cap on external search queries for this request
        #[arg(long)]
        max_external_queries: Option<u32>,
        /// Bounded iterative retrieval rounds (1 disables follow-up queries)
        #[arg(long)]
        max_acquisition_rounds: Option<u32>,
        /// Agent session id: turns are recorded for a later follow-up
        #[arg(long)]
        session: Option<String>,
        /// Comma-separated requirements to focus on (follow-up turns)
        #[arg(long)]
        focus_requirements: Option<String>,
        /// Cap on total estimated cost for this request (major units)
        #[arg(long)]
        max_cost: Option<f64>,
        /// Maximum number of results returned after the quality gate
        #[arg(long)]
        max_results: Option<u32>,
        /// Client idempotency key (replaying it must not re-bill a search)
        #[arg(long)]
        request_id: Option<String>,
        /// RFC-057: evaluate freshness as of this RFC3339 instant
        /// (historical view; later documents are excluded)
        #[arg(long)]
        as_of: Option<String>,
        /// Assemble an Agent-facing context block under this token budget
        /// (RFC-067/068). The block is deduplicated, keeps every citation
        /// even when content must shrink, and reports truncation honestly.
        #[arg(long)]
        max_context_tokens: Option<usize>,
    },
    /// Fetch, extract and persist a URL
    Fetch {
        /// URL to fetch and persist
        url: String,
        /// Workspace that owns the document
        #[arg(long, default_value = "default")]
        workspace: String,
    },
    /// Import local data into the knowledge store (RFC-029)
    #[command(group(clap::ArgGroup::new("source").required(true).args(["text", "file", "dir"])))]
    Ingest {
        /// Import a text string
        #[arg(long, group = "source")]
        text: Option<String>,
        /// Import a single file
        #[arg(long, group = "source")]
        file: Option<PathBuf>,
        /// Import a directory (one document per file)
        #[arg(long, group = "source")]
        dir: Option<PathBuf>,
        /// Stable logical identity (recommended for files you will update)
        #[arg(long)]
        source_key: Option<String>,
        /// Human title (defaults to the file name or first line)
        #[arg(long)]
        title: Option<String>,
        /// Content type hint (sniffed when omitted)
        #[arg(long)]
        content_type: Option<String>,
        /// Workspace that owns the document
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Recurse into subdirectories (--dir)
        #[arg(long, default_value_t = false)]
        recursive: bool,
        /// Comma-separated extension allowlist (--dir)
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
    },
    /// Document operations
    Document {
        #[command(subcommand)]
        cmd: DocumentCmd,
    },
    /// Memory operations
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// Provider operations
    Provider {
        #[command(subcommand)]
        cmd: ProviderCmd,
    },
    /// Search profile operations (RFC-019 19.5)
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Versioned policy management (RFC-034)
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
    },
    /// Knowledge collections (RFC-030)
    Collection {
        #[command(subcommand)]
        cmd: CollectionCmd,
    },
    /// Entity alias registry (RFC-058)
    Entity {
        #[command(subcommand)]
        cmd: EntityCmd,
    },
    /// Deep-search research workspace (RFC-071)
    Research {
        #[command(subcommand)]
        cmd: ResearchCmd,
    },
    /// Operational statistics (RFC-015 15.3)
    Stats {
        #[command(subcommand)]
        cmd: StatsCmd,
    },
    /// Configuration validation (RFC-019 19.4)
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Data export (RFC-024 24.5) — secrets are never exported
    Export {
        /// Write the export to this file (default: stdout)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Workspace to export
        #[arg(long, default_value = "default")]
        workspace: String,
    },
    /// Evaluation run against the labelled dataset (RFC-015 15.4)
    Eval {
        #[command(subcommand)]
        cmd: EvalCmd,
    },
    /// Deterministic search-quality watchdog (RFC-035)
    Watch {
        #[command(subcommand)]
        cmd: WatchCmd,
    },
    /// Human feedback → graded qrels → evaluation dataset (RFC-033)
    Feedback {
        #[command(subcommand)]
        cmd: FeedbackCmd,
    },
    /// Deterministic structured extraction (RFC-049)
    Structures {
        /// Text to parse (stdin when neither text nor file is given)
        #[arg(long)]
        text: Option<String>,
        /// File to parse
        #[arg(long)]
        file: Option<PathBuf>,
        /// URL to fetch (preview only, nothing is persisted)
        #[arg(long)]
        url: Option<String>,
    },
    /// Cost ledger operations
    Cost {
        #[command(subcommand)]
        cmd: CostCmd,
    },
    /// External evidence promotion (RFC-031)
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCmd,
    },
    /// Trace operations
    Trace {
        #[command(subcommand)]
        cmd: TraceCmd,
    },
    /// Index operations
    Index {
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// Gateway Mode operations
    Gateway {
        #[command(subcommand)]
        cmd: GatewayCmd,
    },
    /// Crawler operations
    Crawler {
        #[command(subcommand)]
        cmd: CrawlerCmd,
    },
    /// Extension operations
    Extension {
        #[command(subcommand)]
        cmd: ExtensionCmd,
    },
    /// Backup operations
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// MCP stdio server (for agent hosts)
    Mcp,
}

#[derive(Subcommand)]
enum DocumentCmd {
    /// List persisted documents (most recent first)
    List {
        /// Maximum number of documents to return
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Filter by source type: web | local_text | local_file | local_directory
        #[arg(long)]
        source_type: Option<String>,
        /// Filter by owning workspace
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show a document and its current version
    Inspect {
        /// Document ID (from `document list`)
        id: String,
    },
    /// Re-fetch the document URL and store a new version
    Refresh {
        /// Document ID to refresh
        id: String,
    },
    /// Delete a document and all of its versions
    Delete {
        /// Document ID to delete
        id: String,
    },
    /// List the deletion-propagation audit trail (RFC-087)
    Deletions {
        /// Filter by document ID
        #[arg(long)]
        document_id: Option<String>,
        /// Filter by owning workspace
        #[arg(long)]
        workspace: Option<String>,
        /// Maximum audit rows to return
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// Search namespaced search memory
    Search {
        /// Free-text memory query
        query: String,
        /// Workspace that owns the memory namespace
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Number of memory hits to return
        #[arg(long, default_value_t = 5)]
        top_k: usize,
    },
    /// Memory storage statistics
    Stats,
    /// Write a reusable memory entry (RFC-013 semantic memory)
    Remember {
        /// Short title
        #[arg(long)]
        title: String,
        /// Memory body (distilled evidence, not a final answer)
        #[arg(long)]
        content: String,
        /// Workspace that owns the memory namespace
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Sharing policy: private | workspace_shared | project_shared | global_public
        #[arg(long, default_value = "workspace_shared")]
        sharing: String,
        /// Comma-separated labels
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Run the memory lifecycle: expire old entries + merge duplicates (RFC-070)
    Lifecycle {
        /// Actually delete (default: dry run, report only)
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum ProviderCmd {
    /// List configured external search providers
    List,
    /// Check a provider's API key in the environment (keys are never stored)
    Add {
        /// Provider name: tavily | brave
        name: String,
    },
    /// Per-provider circuit state and rate-limit status
    Health,
}

#[derive(Subcommand)]
enum CollectionCmd {
    /// List collections in a workspace
    List {
        /// Workspace that owns the collections
        #[arg(long, default_value = "default")]
        workspace: String,
    },
    /// Create (or fetch) a named collection
    Create {
        /// Collection name
        name: String,
        /// Workspace that owns the collection
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Optional description
        #[arg(long)]
        description: Option<String>,
    },
    /// Add a document to a collection
    Add {
        /// Collection id (from `collection list`)
        collection: String,
        /// Document id (from `document list`)
        document: String,
    },
    /// Remove a document from a collection
    Remove {
        /// Collection id
        collection: String,
        /// Document id to remove from the collection
        document: String,
    },
    /// List live document ids in a collection
    Documents {
        /// Collection id
        collection: String,
    },
}

#[derive(Subcommand)]
enum EntityCmd {
    /// List registered entities and aliases
    List,
    /// Register a canonical entity with aliases
    Register {
        /// Canonical name
        #[arg(long)]
        name: String,
        /// Comma-separated aliases
        #[arg(long, value_delimiter = ',')]
        aliases: Vec<String>,
        /// Registration source: operator | heuristic
        #[arg(long, default_value = "operator")]
        source: String,
    },
    /// Resolve text against the alias registry
    Resolve {
        /// Text to resolve
        text: String,
    },
}

/// Deep-search research commands (RFC-071).
#[derive(Subcommand)]
enum ResearchCmd {
    /// Run a bounded deep search (rounds follow the open requirements)
    Run {
        /// Research question
        query: String,
        /// Workspace that owns the research task
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Maximum rounds (1-8)
        #[arg(long, default_value_t = 3)]
        max_rounds: u32,
        /// Search profile id
        #[arg(long, default_value = "general")]
        profile: String,
        /// Resume an existing research workspace
        #[arg(long)]
        session: Option<String>,
    },
    /// Show a research workspace (turns + accumulated evidence)
    Show {
        /// Session id (from `research run`)
        session: String,
    },
}

/// Versioned policy management commands (RFC-034).
#[derive(Subcommand)]
enum PolicyCmd {
    /// Show the active policy versions
    Show,
    /// Show the append-only history of one policy kind
    Versions {
        /// quality | freshness | source
        #[arg(long, default_value = "quality")]
        kind: String,
    },
    /// Submit a new policy version from a JSON file
    Set {
        /// quality | freshness | source
        #[arg(long, default_value = "quality")]
        kind: String,
        /// Path to a JSON policy document
        #[arg(long)]
        file: PathBuf,
        /// Optional audit note
        #[arg(long)]
        notes: Option<String>,
    },
    /// Reactivate an existing policy version
    Rollback {
        /// quality | freshness | source
        #[arg(long, default_value = "quality")]
        kind: String,
        /// Version number to reactivate
        #[arg(long)]
        version: i64,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List built-in search profiles
    List,
    /// Show a profile's versioned settings
    Inspect {
        /// Profile ID (e.g. general, fast, deep, latest)
        id: String,
    },
}

#[derive(Subcommand)]
enum StatsCmd {
    /// Retrieval / reuse rates and exact-cache counters
    Retrieval,
    /// Cost savings, CPQPA and reuse views
    Savings,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Validate the effective configuration (exit 2 on errors)
    Validate,
}

#[derive(Subcommand)]
enum FeedbackCmd {
    /// Record an append-only judgement on one retrieved result
    Add {
        /// The query that produced the result
        #[arg(long)]
        query: String,
        /// Result URL / canonical reference
        #[arg(long)]
        url: String,
        /// Human verdict
        #[arg(long)]
        verdict: String,
        /// Workspace scope
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Search task id, when known
        #[arg(long)]
        task: Option<String>,
        /// Optional local note (never sent to a provider)
        #[arg(long)]
        note: Option<String>,
    },
    /// List recorded judgements, newest first
    List {
        /// Workspace scope
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Maximum rows
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Aggregate accepted qrels (disputed pairs are excluded)
    Qrels {
        /// Workspace scope
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Votes a verdict needs to become a qrel
        #[arg(long, default_value_t = 2)]
        min_agreement: usize,
    },
    /// Export a graded evaluation dataset (JSONL) from accepted feedback
    Dataset {
        /// Workspace scope
        #[arg(long, default_value = "default")]
        workspace: String,
        /// Votes a verdict needs to become a qrel
        #[arg(long, default_value_t = 2)]
        min_agreement: usize,
        /// Write JSONL to this file (default: stdout)
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum WatchCmd {
    /// Run every applicable check and append one snapshot
    Run,
    /// Show the most recent snapshots
    History {
        /// Maximum snapshots to show
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum EvalCmd {
    /// Run the labelled evaluation dataset through the real search service
    Run {
        /// Dataset JSONL path
        #[arg(long, default_value = "fixtures/evaluations/dataset-v1.jsonl")]
        dataset: PathBuf,
        /// Evaluate only the first N questions (0 = all)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Write the full JSON report to this file
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Replay recent recorded traces offline and aggregate decisions (RFC-086)
    Replay {
        /// Maximum traces to replay
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Workspace scope
        #[arg(long)]
        workspace: Option<String>,
        /// Quality threshold override as JSON (RFC-075)
        #[arg(long)]
        policy: Option<String>,
    },
}

#[derive(Subcommand)]
enum CostCmd {
    /// Ledger summary (estimates; real bills arrive via reconcile)
    Summary,
    /// Estimated cost grouped by agent
    Agents,
    /// Estimated cost grouped by provider
    Providers,
    /// Estimated cost grouped by operation
    Operations,
    /// Savings, CPQPA and reuse views
    Savings,
    /// Budget reserve → settle audit trail
    Reservations,
    /// Import a real provider bill for a cost event (estimates stay unchanged)
    Reconcile {
        /// cost_events row id
        #[arg(long)]
        event: String,
        /// Actual billed amount in major units
        #[arg(long)]
        actual: f64,
        /// Billing source label
        #[arg(long, default_value = "provider_billing")]
        source: String,
    },
    /// Write the ledger summary to a file (default: stdout)
    Export {
        /// Output file path
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// External acquisitions and their promotion state
    List {
        /// Workspace scope (default: all)
        #[arg(long)]
        workspace: Option<String>,
        /// Maximum acquisitions to list
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Acquisition / promotion / reuse counts and the compound ratio
    Stats {
        /// Workspace scope (default: all)
        #[arg(long)]
        workspace: Option<String>,
    },
}

#[derive(Subcommand)]
enum TraceCmd {
    /// Show a decision trace with structured stages
    Show {
        /// Trace ID (from a search response or `GET /traces`)
        trace_id: String,
    },
}

#[derive(Subcommand)]
enum IndexCmd {
    /// Index consistency between Tantivy and SQLite
    Status,
    /// Alias of `index status`
    Stats,
    /// Rebuild the search index from persisted documents
    Rebuild,
    /// Verify consistency and exit non-zero when drifted
    Verify,
}

#[derive(Subcommand)]
enum GatewayCmd {
    /// Start the HTTP Gateway Mode server (Tavily-compatible)
    Start {
        /// Port to bind
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Host / interface to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Show the configured gateway address (the server may be down)
    Status,
}

#[derive(Subcommand)]
enum CrawlerCmd {
    /// List registered site crawlers
    List,
    /// Show which crawler handles a domain
    Inspect {
        /// Domain to inspect
        domain: String,
    },
    /// Fetch a URL with the selected crawler and report success
    Test {
        /// URL to fetch
        url: String,
    },
    /// Fetch a URL and print extracted title / text / markdown
    Preview {
        /// URL to preview
        url: String,
    },
    /// Recent crawler failures
    Failures,
    /// Per-crawler circuit status
    Status,
    /// Validate all crawler manifests
    Validate,
}

#[derive(Subcommand)]
enum ExtensionCmd {
    /// List loaded extensions and their state
    List {
        /// Show only quarantined manifests and their errors
        #[arg(long, default_value_t = false)]
        quarantined: bool,
    },
    /// Validate all extension manifests
    Validate,
    /// Inspect one extension manifest
    Inspect {
        /// Extension ID
        id: String,
    },
}

#[derive(Subcommand)]
enum BackupCmd {
    /// Create a consistent backup (SQLite + redacted config + manifests)
    Create,
    /// Restore a backup after checksum and integrity verification
    Restore {
        /// Backup directory (or a legacy .db file)
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    // Bare `searchos` prints the full help and exits 0 instead of a usage error.
    if std::env::args_os().len() == 1 {
        use clap::CommandFactory;
        Cli::command().print_help().expect("print help to stdout");
        std::process::exit(0);
    }
    let cli = Cli::parse();
    let format = cli.format;
    // -v raises the log level above the default; RUST_LOG still wins.
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    // Build telemetry BEFORE installing the tracing subscriber so the OTLP
    // bridge can be attached as a layer. The guard must outlive `execute` and
    // be dropped explicitly before `process::exit` (which skips destructors),
    // otherwise the graceful shutdown loses its final span/metric flush.
    // Use the report-style loader here so `run()` remains the single place
    // that prints config warnings (it loads the config again). No duplicate
    // stderr output on every command.
    let (startup_config, _, _) = searchos_core::load_config_report_full(cli.config.as_deref());
    let otlp_config = startup_config.observability.otlp_config();
    let otlp = match searchos_observability::otlp::init(&otlp_config) {
        Ok(handle) => handle,
        Err(e) => {
            eprintln!("searchos telemetry warning: OTLP export disabled: {e}");
            None
        }
    };

    let code = match otlp {
        Some(handle) => {
            let searchos_observability::OtlpHandle { layer, guard } = handle;
            searchos_observability::logging::init_with_otlp(level, layer);
            let code = execute(cli, format).await;
            drop(guard);
            code
        }
        None => {
            searchos_observability::logging::init_with_default(level);
            execute(cli, format).await
        }
    };
    std::process::exit(code);
}

/// Runs the command and maps errors to the documented exit codes.
async fn execute(cli: Cli, format: Format) -> i32 {
    match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            let code = match &e {
                SearchOsError::Validation(_) => 2,
                SearchOsError::Budget(_) => 3,
                SearchOsError::Policy(_) | SearchOsError::Security(_) => 4,
                SearchOsError::Provider(_)
                | SearchOsError::Retrieval(_)
                | SearchOsError::Timeout(_) => 5,
                _ => 1,
            };
            if format == Format::Json {
                // Machine-readable errors on stderr so stdout stays parseable.
                // `code` is the process exit code; `error_code` is the shared
                // uppercase transport table (INV-30-19).
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": code,
                        "error_code": e.class().code,
                    })
                );
            } else {
                eprintln!("error: {e}");
            }
            code
        }
    }
}

fn load_config(cli: &Cli) -> SearchOsConfig {
    let mut cfg = searchos_core::load_config(cli.config.as_deref());
    if let Some(dir) = &cli.data_dir {
        cfg.app.data_dir = dir.clone();
    }
    cfg
}

/// Walks a directory into batch ingest items (RFC-029 6.2 `--dir`).
/// Dotfiles are skipped; the extension allowlist is applied when non-empty;
/// each item's `source_key` is its path relative to `dir`.
fn build_dir_items(
    dir: &Path,
    recursive: bool,
    ext: &[String],
    base_req: IngestRequest,
) -> Result<Vec<IngestBatchItem>, SearchOsError> {
    fn validation(field: &str, message: &str) -> SearchOsError {
        SearchOsError::Validation(ValidationError {
            field: field.to_string(),
            message: message.to_string(),
        })
    }

    let mut items = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current).map_err(|e| validation("dir", &e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| validation("dir", &e.to_string()))?;
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if recursive {
                    stack.push(path);
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            if !ext.is_empty() {
                let matches = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| ext.iter().any(|x| x.eq_ignore_ascii_case(e)))
                    .unwrap_or(false);
                if !matches {
                    continue;
                }
            }
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut req = base_req.clone();
            req.source_key = Some(rel);
            req.title = Some(file_name);
            items.push(IngestBatchItem {
                request: req,
                payload: IngestPayload::File(path),
            });
        }
    }
    items.sort_by(|a, b| a.request.source_key.cmp(&b.request.source_key));
    Ok(items)
}

async fn runtime(cli: &Cli) -> Result<Arc<SearchOsRuntime>, SearchOsError> {
    let cfg = load_config(cli);
    SearchOsRuntime::init(cfg).await
}

async fn run(cli: Cli) -> Result<(), SearchOsError> {
    let format = cli.format;
    let cli = &cli;
    let config_now = load_config(cli);
    let Cli { command, .. } = &cli;
    let cli_ref = cli;
    match command {
        Command::Init => {
            let cfg = config_now.clone();
            std::fs::create_dir_all(&cfg.app.data_dir)
                .map_err(|e| SearchOsError::Storage(format!("create data dir: {e}")))?;
            let rt = SearchOsRuntime::init(cfg.clone()).await?;
            print(
                &serde_json::json!({
                    "status": "initialized",
                    "data_dir": cfg.app.data_dir.to_string_lossy(),
                    "database": cfg.data_db_path().to_string_lossy(),
                    "schema_version": rt.sqlite.schema_version()?,
                }),
                format,
            );
            Ok(())
        }
        Command::Doctor => {
            let rt = runtime(cli_ref).await?;
            let (tantivy, sqlite) = rt.index_consistency().await?;
            let providers = rt.provider_registry.ids();
            print(
                &serde_json::json!({
                    "version": searchos_core::VERSION,
                    "data_dir": rt.config.app.data_dir.to_string_lossy(),
                    "database_ok": true,
                    "schema_version": rt.sqlite.schema_version()?,
                    "index": {
                        "fulltext_backend": rt.fulltext_backend(),
                        "fulltext_chunks": tantivy,
                        "tantivy_chunks": tantivy,
                        "sqlite_chunks": sqlite,
                        "consistent": tantivy == sqlite,
                    },
                    "providers": providers,
                    "tavily_key_present": rt.config.tavily_api_key().is_some(),
                    "brave_key_present": rt.config.brave_api_key().is_some(),
                    "crawlers": rt.crawlers.list().len(),
                    "extensions": rt.extensions.list().len(),
                }),
                format,
            );
            Ok(())
        }
        Command::Version => {
            print(
                &serde_json::json!({
                    "name": "searchos",
                    "version": searchos_core::VERSION,
                    "rfc_suite": "1.0",
                }),
                format,
            );
            Ok(())
        }
        Command::Completions { shell } => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "searchos", &mut std::io::stdout());
            Ok(())
        }
        Command::Search {
            query,
            profile,
            mode,
            workspace,
            max_external_queries,
            max_acquisition_rounds,
            session,
            focus_requirements,
            max_cost,
            max_results,
            request_id,
            max_context_tokens,
            as_of,
        } => {
            let rt = runtime(cli_ref).await?;
            let mode = parse_mode(mode)?;
            let mut req = SearchRequest::new(
                query,
                ProfileId::new(profile),
                mode,
                WorkspaceId::new(workspace),
            )?;
            if let Some(n) = max_external_queries {
                req.budget.max_external_queries = *n;
            }
            if let Some(n) = max_acquisition_rounds {
                req.budget.max_acquisition_rounds = (*n).clamp(1, 5);
            }
            if let Some(sid) = session.as_deref() {
                if !sid.trim().is_empty() {
                    req.session_id = Some(sid.to_string());
                }
            }
            if let Some(focus) = focus_requirements.as_deref() {
                req.focus_requirements = focus
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            if let Some(c) = max_cost {
                req.budget.max_total_cost_micros = (c * 1_000_000.0).round() as i64;
            }
            if let Some(rid) = request_id {
                req = req.with_request_id(rid.clone());
            }
            if let Some(value) = as_of.as_deref() {
                req.constraints.as_of = Some(
                    chrono::DateTime::parse_from_rfc3339(value)
                        .map_err(|e| SearchOsError::validation("as_of", e.to_string()))?
                        .with_timezone(&chrono::Utc),
                );
            }
            if let Some(max_tokens) = max_context_tokens {
                let budget = searchos_domain::context::ContextBudget {
                    max_tokens: *max_tokens,
                    ..Default::default()
                };
                if let Err(message) = budget.validate() {
                    return Err(searchos_domain::SearchOsError::validation(
                        "max-context-tokens",
                        message,
                    ));
                }
                req.context_budget = Some(budget);
            }
            if let Some(max) = max_results {
                req = req.with_max_results(*max);
            }
            let mut resp = rt.application.search_service.search(req).await?;
            if let Some(max) = max_results {
                resp.results
                    .truncate(searchos_domain::request::clamp_max_results(*max) as usize);
            }
            print(&serde_json::to_value(&resp).unwrap_or_default(), format);
            Ok(())
        }
        Command::Fetch { url, workspace } => {
            let rt = runtime(cli_ref).await?;
            let doc = rt
                .application
                .fetch_service
                .fetch_and_persist(url, &WorkspaceId::new(workspace))
                .await?;
            print(&serde_json::to_value(&doc).unwrap_or_default(), format);
            Ok(())
        }
        Command::Ingest {
            text,
            file,
            dir,
            source_key,
            title,
            content_type,
            workspace,
            recursive,
            ext,
        } => {
            let rt = runtime(cli_ref).await?;
            let ws = WorkspaceId::new(workspace.clone());
            let mut req = IngestRequest::new(ws);
            req.source_key = source_key.clone();
            req.title = title.clone();
            req.content_type = content_type.clone();

            if let Some(t) = text {
                let doc = rt.application.ingest_service.ingest_text(req, t).await?;
                print(&serde_json::to_value(&doc).unwrap_or_default(), format);
            } else if let Some(f) = file {
                let doc = rt.application.ingest_service.ingest_file(req, f).await?;
                print(&serde_json::to_value(&doc).unwrap_or_default(), format);
            } else if let Some(d) = dir {
                let items = build_dir_items(d, *recursive, ext, req)?;
                let result = rt.application.ingest_service.ingest_batch(items).await?;
                print(&serde_json::to_value(&result).unwrap_or_default(), format);
            }
            Ok(())
        }
        Command::Document { cmd } => {
            let rt = runtime(cli_ref).await?;
            let repo = repos::DocumentRepo::new(&rt.sqlite);
            match cmd {
                DocumentCmd::List {
                    limit,
                    source_type,
                    workspace,
                } => {
                    let docs =
                        repo.list_filtered(*limit, source_type.as_deref(), workspace.as_deref())?;
                    print(&serde_json::to_value(&docs).unwrap_or_default(), format);
                }
                DocumentCmd::Inspect { id } => {
                    let doc_id = searchos_domain::ids::DocumentId::new(id);
                    let doc = repo
                        .get(&doc_id)?
                        .ok_or_else(|| SearchOsError::Retrieval("document not found".into()))?;
                    let version = repo.get_current_version(&doc_id)?;
                    print(
                        &serde_json::json!({ "document": doc, "current_version": version }),
                        format,
                    );
                }
                DocumentCmd::Refresh { id } => {
                    let doc_id = searchos_domain::ids::DocumentId::new(id);
                    let doc = repo
                        .get(&doc_id)?
                        .ok_or_else(|| SearchOsError::Retrieval("document not found".into()))?;
                    // Refresh keeps the document's owning workspace (RFC-017
                    // isolation): a hard-coded default would move the new
                    // version into another workspace and corrupt quotas.
                    let refreshed = rt
                        .application
                        .fetch_service
                        .fetch_and_persist(&doc.canonical_url, &WorkspaceId::new(&doc.workspace_id))
                        .await?;
                    print(
                        &serde_json::to_value(&refreshed).unwrap_or_default(),
                        format,
                    );
                }
                DocumentCmd::Delete { id } => {
                    // RFC-087: route through the runtime so the deletion is
                    // drained, cache-invalidated and AUDITED — a bare
                    // repo.delete would leave the index stale with no audit
                    // trail.
                    rt.delete_document(&searchos_domain::ids::DocumentId::new(id.clone()))
                        .await?;
                    print(&serde_json::json!({ "deleted": id }), format);
                }
                DocumentCmd::Deletions {
                    document_id,
                    workspace,
                    limit,
                } => {
                    let audit = repos::DeletionAuditRepo::new(&rt.sqlite).list(
                        *limit,
                        workspace.as_deref(),
                        document_id.as_deref(),
                    )?;
                    print(&serde_json::to_value(&audit).unwrap_or_default(), format);
                }
            }
            Ok(())
        }
        Command::Memory { cmd } => {
            let rt = runtime(cli_ref).await?;
            match cmd {
                MemoryCmd::Search {
                    query,
                    workspace,
                    top_k,
                } => {
                    let reader = MemoryNamespace {
                        workspace: WorkspaceId::new(workspace),
                        project: None,
                        agent: None,
                    };
                    let hits = rt
                        .application
                        .memory_service
                        .search(query, &reader, *top_k)
                        .await?;
                    print(&serde_json::json!({ "results": hits }), format);
                }
                MemoryCmd::Stats => {
                    let stats = rt.application.memory_service.stats().await?;
                    print(&serde_json::to_value(stats).unwrap_or_default(), format);
                }
                MemoryCmd::Lifecycle { apply } => {
                    let expired = rt.application.memory_service.expire(*apply).await?;
                    let consolidated = rt.application.memory_service.consolidate(*apply).await?;
                    print(
                        &serde_json::json!({
                            "expired": expired.expired.len(),
                            "expired_ids": expired.expired,
                            "merged_groups": consolidated.merged_groups,
                            "removed_duplicates": consolidated.removed_duplicates,
                            "applied": *apply,
                        }),
                        format,
                    );
                }
                MemoryCmd::Remember {
                    title,
                    content,
                    workspace,
                    sharing,
                    tags,
                } => {
                    let sharing = match sharing.as_str() {
                        "private" => SharingPolicy::Private,
                        "workspace_shared" | "workspace" => SharingPolicy::WorkspaceShared,
                        "project_shared" | "project" => SharingPolicy::ProjectShared,
                        "global_public" | "global" => SharingPolicy::GlobalPublic,
                        other => {
                            return Err(SearchOsError::validation(
                                "sharing",
                                format!("unknown sharing policy {other}"),
                            ))
                        }
                    };
                    let namespace = MemoryNamespace {
                        workspace: WorkspaceId::new(workspace.clone()),
                        project: None,
                        agent: None,
                    };
                    let entry = rt
                        .application
                        .memory_service
                        .remember(namespace, sharing, title, content, None, tags)
                        .await?;
                    print(&serde_json::to_value(entry).unwrap_or_default(), format);
                }
            }
            Ok(())
        }
        Command::Provider { cmd } => {
            let rt = runtime(cli_ref).await?;
            match cmd {
                ProviderCmd::List => {
                    let ids = rt.provider_registry.ids();
                    print(&serde_json::json!({ "providers": ids }), format);
                }
                ProviderCmd::Add { name } => {
                    // Register (config via env vars; secrets never stored in
                    // config or DB — RFC-014 14.3).
                    let known = ["tavily", "brave"];
                    if !known.contains(&name.as_str()) {
                        return Err(SearchOsError::validation(
                            "provider",
                            format!("unknown provider {name}; known: tavily, brave"),
                        ));
                    }
                    let key_env = if name == "tavily" {
                        "TAVILY_API_KEY"
                    } else {
                        "BRAVE_API_KEY"
                    };
                    print(
                        &serde_json::json!({
                            "provider": name,
                            "status": if std::env::var(key_env).is_ok() { "available" } else { "missing_key" },
                            "secret_env": key_env,
                        }),
                        format,
                    );
                }
                ProviderCmd::Health => {
                    let mut out = Vec::new();
                    for id in rt.provider_registry.ids() {
                        let (state, rate_limited) = rt
                            .provider_registry
                            .health(&id)
                            .unwrap_or((searchos_provider::circuit::CircuitState::Closed, false));
                        let mut entry = serde_json::json!({
                            "provider_id": id,
                            "state": format!("{state:?}"),
                            "rate_limited": rate_limited,
                        });
                        // Per-key rotation status (RFC-009 9.7), masked.
                        if let Some(p) = rt.provider_registry.get(&id) {
                            if let Some(keys) = p.search.as_ref().and_then(|s| s.key_status()) {
                                entry["keys"] = serde_json::json!(keys
                                    .iter()
                                    .map(|k| serde_json::json!({
                                        "index": k.index,
                                        "masked_key": k.masked_key,
                                        "state": k.state,
                                        "available_at": k.available_at,
                                        "consecutive_failures": k.consecutive_failures,
                                    }))
                                    .collect::<Vec<_>>());
                            }
                        }
                        out.push(entry);
                    }
                    print(&serde_json::json!({ "providers": out }), format);
                }
            }
            Ok(())
        }
        Command::Profile { cmd } => {
            let registry = searchos_policy::ProfileRegistry::builtin();
            match cmd {
                ProfileCmd::List => {
                    print(&serde_json::json!({ "profiles": registry.all() }), format);
                }
                ProfileCmd::Inspect { id } => {
                    let p = registry.get(id).ok_or_else(|| {
                        SearchOsError::validation("profile", format!("unknown profile {id}"))
                    })?;
                    print(&serde_json::to_value(p).unwrap_or_default(), format);
                }
            }
            Ok(())
        }
        Command::Policy { cmd } => {
            let rt = runtime(cli_ref).await?;
            let repo = repos::PolicyRepo::new(&rt.sqlite);
            match cmd {
                PolicyCmd::Show => {
                    let mut active = Vec::new();
                    for kind in searchos_application::policy::POLICY_KINDS {
                        if let Some(row) = repo.active(kind)? {
                            active.push(row);
                        }
                    }
                    print(
                        &serde_json::json!({
                            "policies": active,
                            "live": rt.policy_state.versions(),
                        }),
                        format,
                    );
                    Ok(())
                }
                PolicyCmd::Versions { kind } => {
                    let pkind = searchos_application::policy::PolicyKind::parse(kind)
                        .ok_or_else(|| SearchOsError::validation("kind", "UNKNOWN_POLICY_KIND"))?;
                    let rows = repo.versions(pkind.as_str())?;
                    print(
                        &serde_json::json!({ "kind": kind, "versions": rows }),
                        format,
                    );
                    Ok(())
                }
                PolicyCmd::Set { kind, file, notes } => {
                    let pkind = searchos_application::policy::PolicyKind::parse(kind)
                        .ok_or_else(|| SearchOsError::validation("kind", "UNKNOWN_POLICY_KIND"))?;
                    let raw = std::fs::read_to_string(file)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let value: serde_json::Value = serde_json::from_str(&raw)
                        .map_err(|e| SearchOsError::validation("policy", e.to_string()))?;
                    let payload = searchos_application::policy::validate_payload(pkind, &value)?;
                    let row = repo.submit(pkind.as_str(), &payload, notes.as_deref())?;
                    rt.activate_policy(pkind, &row).await?;
                    print(&serde_json::to_value(&row).unwrap_or_default(), format);
                    Ok(())
                }
                PolicyCmd::Rollback { kind, version } => {
                    let pkind = searchos_application::policy::PolicyKind::parse(kind)
                        .ok_or_else(|| SearchOsError::validation("kind", "UNKNOWN_POLICY_KIND"))?;
                    let row = repo.activate(pkind.as_str(), *version)?.ok_or_else(|| {
                        SearchOsError::validation("version", "UNKNOWN_POLICY_VERSION")
                    })?;
                    rt.activate_policy(pkind, &row).await?;
                    print(&serde_json::to_value(&row).unwrap_or_default(), format);
                    Ok(())
                }
            }
        }
        Command::Research { cmd } => match cmd {
            ResearchCmd::Run {
                query,
                workspace,
                max_rounds,
                profile,
                session,
            } => {
                let rt = runtime(cli_ref).await?;
                let req = searchos_application::research::research_request(
                    query,
                    profile,
                    searchos_domain::request::SearchMode::LocalFirst,
                    workspace,
                    100_000,
                    5,
                )?;
                let session_id = session
                    .clone()
                    .unwrap_or_else(|| searchos_domain::new_id("research"));
                let run = rt
                    .research
                    .deep_search(req, &session_id, *max_rounds)
                    .await?;
                print(&serde_json::to_value(&run).unwrap_or_default(), format);
                Ok(())
            }
            ResearchCmd::Show { session } => {
                let rt = runtime(cli_ref).await?;
                let workspace = rt
                    .session_store
                    .get(session)
                    .ok_or_else(|| SearchOsError::validation("session", "UNKNOWN_SESSION"))?;
                print(
                    &serde_json::to_value(&workspace).unwrap_or_default(),
                    format,
                );
                Ok(())
            }
        },
        Command::Entity { cmd } => match cmd {
            EntityCmd::List => {
                let rt = runtime(cli_ref).await?;
                let aliases = searchos_application::entity::EntityLookup::alias_count(
                    rt.entity_registry.as_ref(),
                );
                let entities = rt.entity_store.entities().await?;
                print(
                    &serde_json::json!({ "alias_count": aliases, "entities": entities }),
                    format,
                );
                Ok(())
            }
            EntityCmd::Register {
                name,
                aliases,
                source,
            } => {
                let rt = runtime(cli_ref).await?;
                let source = match source.as_str() {
                    "operator" => searchos_domain::entity::EntitySource::Operator,
                    "heuristic" => searchos_domain::entity::EntitySource::Heuristic,
                    other => {
                        return Err(SearchOsError::validation(
                            "source",
                            format!("unknown {other}"),
                        ))
                    }
                };
                let entity = searchos_domain::entity::Entity::new(name, aliases.clone(), source);
                let conflicts = rt.register_entity(&entity).await?;
                print(
                    &serde_json::json!({ "entity": entity, "conflicts": conflicts }),
                    format,
                );
                Ok(())
            }
            EntityCmd::Resolve { text } => {
                let rt = runtime(cli_ref).await?;
                let matched = searchos_application::entity::EntityLookup::resolve(
                    rt.entity_registry.as_ref(),
                    text,
                );
                print(&serde_json::to_value(matched).unwrap_or_default(), format);
                Ok(())
            }
        },
        Command::Collection { cmd } => match cmd {
            CollectionCmd::List { workspace } => {
                let rt = runtime(cli_ref).await?;
                let rows = repos::CollectionRepo::new(&rt.sqlite).list(Some(workspace))?;
                print(&serde_json::to_value(rows).unwrap_or_default(), format);
                Ok(())
            }
            CollectionCmd::Create {
                name,
                workspace,
                description,
            } => {
                let rt = runtime(cli_ref).await?;
                let collection = searchos_domain::collection::Collection::new(
                    &WorkspaceId::new(workspace),
                    name,
                    description.clone(),
                );
                let saved = repos::CollectionRepo::new(&rt.sqlite).ensure(&collection)?;
                print(&serde_json::to_value(saved).unwrap_or_default(), format);
                Ok(())
            }
            CollectionCmd::Add {
                collection,
                document,
            } => {
                let rt = runtime(cli_ref).await?;
                repos::CollectionRepo::new(&rt.sqlite).add_document(
                    &searchos_domain::collection::CollectionId(collection.clone()),
                    &searchos_domain::ids::DocumentId::new(document.clone()),
                )?;
                print(
                    &serde_json::json!({ "added": document, "collection": collection }),
                    format,
                );
                Ok(())
            }
            CollectionCmd::Remove {
                collection,
                document,
            } => {
                let rt = runtime(cli_ref).await?;
                repos::CollectionRepo::new(&rt.sqlite).remove_document(
                    &searchos_domain::collection::CollectionId(collection.clone()),
                    &searchos_domain::ids::DocumentId::new(document.clone()),
                )?;
                print(
                    &serde_json::json!({ "removed": document, "collection": collection }),
                    format,
                );
                Ok(())
            }
            CollectionCmd::Documents { collection } => {
                let rt = runtime(cli_ref).await?;
                let docs = repos::CollectionRepo::new(&rt.sqlite).documents(
                    &searchos_domain::collection::CollectionId(collection.clone()),
                )?;
                print(
                    &serde_json::json!({ "collection": collection, "documents": docs }),
                    format,
                );
                Ok(())
            }
        },
        Command::Stats { cmd } => {
            let rt = runtime(cli_ref).await?;
            let snapshot = searchos_observability::MetricsRegistry::global().snapshot();
            match cmd {
                StatsCmd::Retrieval => {
                    let (hits, misses) = rt.exact_cache.stats();
                    print(
                        &serde_json::json!({
                            "cache_hit_rate": snapshot.cache_hit_rate,
                            "local_resolution_rate": snapshot.local_resolution_rate,
                            "valid_local_resolution_rate": snapshot.valid_local_resolution_rate,
                            "external_avoidance_rate": snapshot.external_avoidance_rate,
                            "semantic_reuse_rate": snapshot.semantic_reuse_rate,
                            "singleflight_reuse_rate": snapshot.singleflight_reuse_rate,
                            "exact_cache": { "hits": hits, "misses": misses },
                            "status_counts": snapshot.status_counts,
                        }),
                        format,
                    );
                }
                StatsCmd::Savings => {
                    let report = rt
                        .application
                        .cost_service
                        .savings(rt.config.budgets.baseline_per_query_micros)
                        .await?;
                    print(&serde_json::to_value(report).unwrap_or_default(), format);
                }
            }
            Ok(())
        }
        Command::Config { cmd } => match cmd {
            ConfigCmd::Validate => {
                let (cfg, errors, warnings) =
                    searchos_core::load_config_report_full(cli.config.as_deref());
                let valid = errors.is_empty();
                print(
                    &serde_json::json!({
                        "valid": valid,
                        "config_version": cfg.config_version,
                        "api_version": cfg.server.api_version,
                        "errors": errors,
                        "warnings": warnings,
                        "consumed_fields": searchos_core::CONSUMED_CONFIG_FIELDS,
                        "deprecated_fields": searchos_core::DEPRECATED_CONFIG_FIELDS,
                    }),
                    format,
                );
                if !valid {
                    return Err(SearchOsError::validation("config", errors.join("; ")));
                }
                Ok(())
            }
        },
        Command::Export { out, workspace } => {
            let rt = runtime(cli_ref).await?;
            let docs = repos::DocumentRepo::new(&rt.sqlite).list(1000)?;
            let cost = rt
                .application
                .cost_service
                .summary(&Default::default())
                .await?;
            let (tantivy_chunks, sqlite_chunks) = rt.index_consistency().await?;
            let payload = serde_json::json!({
                "format_version": 1,
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "workspace": workspace,
                "config": rt.config.redacted(),
                "documents": docs,
                "cost_summary": cost,
                "index": {
                    "fulltext_backend": rt.fulltext_backend(),
                    "fulltext_chunks": tantivy_chunks,
                    "tantivy_chunks": tantivy_chunks,
                    "sqlite_chunks": sqlite_chunks
                },
                "search_history": "use `searchos cost export` for the ledger; task rows stay in SQLite",
                "secrets": "[never exported]",
            });
            let json = serde_json::to_string_pretty(&payload)
                .map_err(|e| SearchOsError::Internal(e.to_string()))?;
            match out {
                Some(path) => {
                    std::fs::write(path, &json)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    print(
                        &serde_json::json!({ "exported": path.to_string_lossy(), "secrets_redacted": true }),
                        format,
                    );
                }
                None => println!("{json}"),
            }
            Ok(())
        }
        Command::Structures { text, file, url } => {
            let content = match (text, file, url) {
                (Some(text), _, _) => text.clone(),
                (_, Some(path), _) => std::fs::read_to_string(path)
                    .map_err(|e| SearchOsError::Storage(format!("read {}: {e}", path.display())))?,
                (_, _, Some(url)) => {
                    let cli_ref = &cli;
                    let rt = runtime(cli_ref).await?;
                    let preview = rt.application.fetch_service.fetch_preview(url).await?;
                    preview.markdown
                }
                (None, None, None) => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    buffer
                }
            };
            let extraction = searchos_document::structures::extract_structures(&content);
            print(
                &serde_json::to_value(&extraction)
                    .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                format,
            );
            Ok(())
        }
        Command::Feedback { cmd } => {
            let cli_ref = &cli;
            let rt = runtime(cli_ref).await?;
            let feedback = rt.application.feedback_service.as_ref().ok_or_else(|| {
                SearchOsError::Internal("feedback service is not wired".to_string())
            })?;
            match cmd {
                FeedbackCmd::Add {
                    query,
                    url,
                    verdict,
                    workspace,
                    task,
                    note,
                } => {
                    let verdict = searchos_application::feedback::FeedbackVerdict::parse(verdict)
                        .map_err(|e| SearchOsError::validation("verdict", e))?;
                    let event = feedback
                        .submit(searchos_application::feedback::NewFeedback {
                            workspace_id: workspace.clone(),
                            task_id: task.clone(),
                            query: query.clone(),
                            url: url.clone(),
                            verdict,
                            note: note.clone(),
                        })
                        .await?;
                    print(
                        &serde_json::to_value(event)
                            .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                        format,
                    );
                    Ok(())
                }
                FeedbackCmd::List { workspace, limit } => {
                    let events = feedback.list(workspace, *limit).await?;
                    print(
                        &serde_json::to_value(events)
                            .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                        format,
                    );
                    Ok(())
                }
                FeedbackCmd::Qrels {
                    workspace,
                    min_agreement,
                } => {
                    let qrels = feedback.qrels(workspace, *min_agreement).await?;
                    print(
                        &serde_json::to_value(qrels)
                            .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                        format,
                    );
                    Ok(())
                }
                FeedbackCmd::Dataset {
                    workspace,
                    min_agreement,
                    out,
                } => {
                    let dataset = feedback.dataset(workspace, *min_agreement).await?;
                    let mut jsonl = String::new();
                    for question in &dataset.questions {
                        jsonl.push_str(
                            &serde_json::to_string(question)
                                .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                        );
                        jsonl.push('\n');
                    }
                    match out {
                        Some(path) => {
                            std::fs::write(path, jsonl)
                                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                            print(
                                &serde_json::json!({
                                    "dataset_id": dataset.dataset_id,
                                    "questions": dataset.questions.len(),
                                    "path": path.display().to_string(),
                                }),
                                format,
                            );
                        }
                        None => println!("{jsonl}"),
                    }
                    Ok(())
                }
            }
        }
        Command::Watch { cmd } => {
            let cli_ref = &cli;
            let rt = runtime(cli_ref).await?;
            let watch =
                rt.application.watch_service.as_ref().ok_or_else(|| {
                    SearchOsError::Internal("watch service is not wired".to_string())
                })?;
            match cmd {
                WatchCmd::Run => {
                    let report = watch
                        .run()
                        .await
                        .map_err(|e| SearchOsError::Internal(format!("watch run: {e}")))?;
                    let value = serde_json::to_value(&report)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    print(&value, format);
                    Ok(())
                }
                WatchCmd::History { limit } => {
                    let reports = watch
                        .history(*limit)
                        .await
                        .map_err(|e| SearchOsError::Internal(format!("watch history: {e}")))?;
                    let value = serde_json::to_value(reports)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    print(&value, format);
                    Ok(())
                }
            }
        }
        Command::Eval { cmd } => match cmd {
            EvalCmd::Run {
                dataset,
                limit,
                out,
            } => {
                let mut ds = EvaluationDataset::load_jsonl(Path::new(dataset))
                    .map_err(|e| SearchOsError::validation("dataset", e))?;
                if *limit > 0 {
                    ds.questions.truncate(*limit);
                }
                let rt = runtime(cli_ref).await?;
                let svc = Arc::clone(&rt.application.search_service);
                let outcome = searchos_evaluation::run_dataset(&ds, move |r| {
                    let q = r.question.clone();
                    let svc = Arc::clone(&svc);
                    async move {
                        let profile = q.profile.clone().unwrap_or_else(|| "general".into());
                        let req = searchos_domain::request::SearchRequest::new(
                            q.question.clone(),
                            ProfileId::new(profile),
                            searchos_domain::request::SearchMode::LocalFirst,
                            WorkspaceId::new("default"),
                        )
                        .map_err(|e| e.to_string())?;
                        let started = std::time::Instant::now();
                        let resp = svc.search(req).await.map_err(|e| e.to_string())?;
                        let evidence_text = resp
                            .results
                            .iter()
                            .map(|r| {
                                format!(
                                    "{} {} {}",
                                    r.title.as_deref().unwrap_or(""),
                                    r.url,
                                    r.content
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        Ok(EvalQueryResult {
                            quality_status: resp.quality.status.as_str().into(),
                            local_resolved: resp.decision.path == "local"
                                || resp.decision.path == "local_with_refresh",
                            external_calls: if matches!(
                                resp.decision.path.as_str(),
                                "hybrid" | "external"
                            ) {
                                1
                            } else {
                                0
                            },
                            cost_micros: (resp.cost.estimated * 1_000_000.0).round() as i64,
                            latency_ms: started.elapsed().as_millis() as u64,
                            evidence_text,
                            ranked_urls: resp.results.iter().map(|r| r.url.clone()).collect(),
                        })
                    }
                })
                .await
                .map_err(|e| SearchOsError::validation("eval_run", e))?;
                let price = rt.config.budgets.baseline_per_query_micros;
                let external = searchos_evaluation::BaselineResult::external_only_estimate(
                    outcome.total_questions,
                    price,
                    1.0,
                    price / 2,
                );
                let exact = searchos_evaluation::BaselineResult::exact_cache_estimate(
                    outcome.total_questions,
                    outcome.total_questions,
                    price,
                    1.0,
                    price / 2,
                );
                let searchos = searchos_evaluation::BaselineResult::from_run(&outcome, 0);
                let comparison = searchos_evaluation::baselines::compare(external, exact, searchos);
                let report = serde_json::json!({
                    "dataset_id": ds.dataset_id,
                    "questions": outcome.total_questions,
                    "run": outcome,
                    "comparison": comparison,
                    "note": "A/B baselines are analytic estimates; C is a live run (RFC-015 15.4)",
                });
                if let Some(path) = out {
                    std::fs::write(
                        path,
                        serde_json::to_string_pretty(&report)
                            .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                    )
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                }
                print(&report, format);
                Ok(())
            }
            EvalCmd::Replay {
                limit,
                workspace,
                policy,
            } => {
                let rt = runtime(cli_ref).await?;
                let override_value: Option<searchos_application::replay::PolicyOverride> =
                    match policy {
                        Some(raw) => Some(
                            serde_json::from_str(raw)
                                .map_err(|e| SearchOsError::validation("policy", e.to_string()))?,
                        ),
                        None => None,
                    };
                let report = rt
                    .replay
                    .evaluate(*limit, workspace.as_deref(), override_value)
                    .await?;
                print(&serde_json::to_value(&report).unwrap_or_default(), format);
                Ok(())
            }
        },
        Command::Cost { cmd } => {
            let rt = runtime(cli_ref).await?;
            let summary = rt
                .application
                .cost_service
                .summary(&Default::default())
                .await?;
            match cmd {
                CostCmd::Summary => {
                    print(
                        &serde_json::json!({ "summary": summary, "is_estimate": true }),
                        format,
                    );
                }
                CostCmd::Agents => {
                    print(&serde_json::json!({ "by_agent": summary.by_agent }), format);
                }
                CostCmd::Providers => {
                    print(
                        &serde_json::json!({ "by_provider": summary.by_provider }),
                        format,
                    );
                }
                CostCmd::Operations => {
                    print(
                        &serde_json::json!({ "by_operation": summary.by_operation }),
                        format,
                    );
                }
                CostCmd::Savings => {
                    let report = rt
                        .application
                        .cost_service
                        .savings(rt.config.budgets.baseline_per_query_micros)
                        .await?;
                    print(&serde_json::to_value(report).unwrap_or_default(), format);
                }
                CostCmd::Reservations => {
                    let rows = rt.application.cost_service.reservations(50).await?;
                    print(&rows, format);
                }
                CostCmd::Reconcile {
                    event,
                    actual,
                    source,
                } => {
                    if *actual < 0.0 {
                        return Err(SearchOsError::validation("actual", "must be >= 0"));
                    }
                    let micros = (actual * 1_000_000.0).round() as i64;
                    rt.application
                        .cost_service
                        .reconcile(event, micros, source, "USD")
                        .await?;
                    print(
                        &serde_json::json!({
                            "reconciled": event,
                            "actual_cost_micros": micros,
                            "source": source,
                            "estimate_unchanged": true,
                        }),
                        format,
                    );
                }
                CostCmd::Export { out } => {
                    let json = serde_json::to_string_pretty(&summary)
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    match out {
                        Some(path) => {
                            std::fs::write(path, json)
                                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                            print(
                                &serde_json::json!({"exported": path.to_string_lossy()}),
                                format,
                            );
                        }
                        None => println!("{json}"),
                    }
                }
            }
            Ok(())
        }
        Command::Evidence { cmd } => {
            let rt = runtime(cli_ref).await?;
            let repo = repos::ExternalEvidenceRepo::new(&rt.sqlite);
            match cmd {
                EvidenceCmd::List { workspace, limit } => {
                    let acquisitions = repo.list_acquisitions(workspace.as_deref(), *limit)?;
                    let stats = repo.stats(workspace.as_deref())?;
                    let report = serde_json::json!({
                        "acquisitions": acquisitions,
                        "stats": stats,
                    });
                    print(&report, format);
                    Ok(())
                }
                EvidenceCmd::Stats { workspace } => {
                    let stats = repo.stats(workspace.as_deref())?;
                    let acquisitions = stats
                        .get("acquisitions")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let reuse = stats
                        .get("reuse_events")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let report = serde_json::json!({
                        "stats": stats,
                        "compound_ratio": if acquisitions > 0 {
                            reuse as f64 / acquisitions as f64
                        } else {
                            0.0
                        },
                    });
                    print(&report, format);
                    Ok(())
                }
            }
        }
        Command::Trace { cmd } => {
            let rt = runtime(cli_ref).await?;
            match cmd {
                TraceCmd::Show { trace_id } => {
                    let repo = repos::DecisionRepo::new(&rt.sqlite);
                    let trace = repo.get_trace(trace_id)?.ok_or_else(|| {
                        SearchOsError::Retrieval(format!("trace {trace_id} not found"))
                    })?;
                    print(&serde_json::to_value(trace).unwrap_or_default(), format);
                }
            }
            Ok(())
        }
        Command::Index { cmd } => {
            let rt = runtime(cli_ref).await?;
            match cmd {
                IndexCmd::Status => {
                    let (index_chunks, sqlite) = rt.index_consistency().await?;
                    print(
                        &serde_json::json!({
                            "fulltext_backend": rt.fulltext_backend(),
                            "fulltext_chunks": index_chunks,
                            "tantivy_version": rt.tantivy.as_ref().map(|t| t.version()),
                            "tantivy_chunks": index_chunks,
                            "sqlite_chunks": sqlite,
                            "consistent": index_chunks == sqlite,
                        }),
                        format,
                    );
                }
                IndexCmd::Stats => {
                    // Alias of `index status` (MVP §8.4).
                    let (index_chunks, sqlite) = rt.index_consistency().await?;
                    print(
                        &serde_json::json!({
                            "fulltext_backend": rt.fulltext_backend(),
                            "fulltext_chunks": index_chunks,
                            "tantivy_version": rt.tantivy.as_ref().map(|t| t.version()),
                            "tantivy_chunks": index_chunks,
                            "sqlite_chunks": sqlite,
                            "consistent": index_chunks == sqlite,
                        }),
                        format,
                    );
                }
                IndexCmd::Rebuild => {
                    let n = rt.rebuild_index().await?;
                    print(&serde_json::json!({ "rebuilt_documents": n }), format);
                }
                IndexCmd::Verify => {
                    let (tantivy, sqlite) = rt.index_consistency().await?;
                    let consistent = tantivy == sqlite;
                    print(
                        &serde_json::json!({
                            "consistent": consistent,
                            "tantivy_chunks": tantivy,
                            "sqlite_chunks": sqlite,
                            "action": if consistent { "none" } else { "run `searchos index rebuild`" },
                        }),
                        format,
                    );
                    if !consistent {
                        return Err(SearchOsError::Retrieval(
                            "index inconsistency detected (rerun with `index rebuild`)".into(),
                        ));
                    }
                }
            }
            Ok(())
        }
        Command::Gateway { cmd } => {
            match cmd {
                GatewayCmd::Start { port, host } => {
                    let mut cfg = config_now.clone();
                    cfg.server.enabled = true;
                    cfg.server.port = *port;
                    cfg.server.host = host.clone();
                    let rt = SearchOsRuntime::init(cfg).await?;
                    print(
                        &serde_json::json!({
                            "status": "starting",
                            "address": format!("http://{}:{}", rt.config.server.host, rt.config.server.port),
                        }),
                        format,
                    );
                    searchos_interface_http::serve(searchos_interface_http::AppState::new(rt))
                        .await
                        .map_err(SearchOsError::Storage)?;
                    Ok(())
                }
                GatewayCmd::Status => {
                    // Status is a file probe (the gateway may be down).
                    let cfg = config_now.clone();
                    print(
                        &serde_json::json!({
                            "configured_address": format!("{}:{}", cfg.server.host, cfg.server.port),
                            "mode": "gateway",
                        }),
                        format,
                    );
                    Ok(())
                }
            }
        }
        Command::Crawler { cmd } => {
            let rt = runtime(cli_ref).await?;
            let crawlers = &rt.crawlers;
            match cmd {
                CrawlerCmd::List => {
                    let list: Vec<_> = crawlers
                        .list()
                        .into_iter()
                        .map(|m| {
                            serde_json::json!({
                                "id": m.id, "version": m.version, "domains": m.match_spec.domains,
                            })
                        })
                        .collect();
                    print(&serde_json::json!({ "crawlers": list }), format);
                }
                CrawlerCmd::Inspect { domain } => {
                    let sel = crawlers.select(domain);
                    print(
                        &serde_json::json!({
                            "domain": domain,
                            "selection": match sel {
                                searchos_crawler::SelectedCrawler::SiteSpecific(id) => format!("site_specific:{id}"),
                                searchos_crawler::SelectedCrawler::Generic => "generic".into(),
                                searchos_crawler::SelectedCrawler::FallbackFetch => "fallback_fetch".into(),
                            },
                        }),
                        format,
                    );
                }
                CrawlerCmd::Test { url } => {
                    let res = rt.application.fetch_service.fetch_preview(url).await;
                    match res {
                        Ok(c) => print(
                            &serde_json::json!({"url": url, "ok": true, "chars": c.text.len()}),
                            format,
                        ),
                        Err(e) => print(
                            &serde_json::json!({"url": url, "ok": false, "error": e.to_string()}),
                            format,
                        ),
                    }
                }
                CrawlerCmd::Preview { url } => {
                    let c = rt.application.fetch_service.fetch_preview(url).await?;
                    print(
                        &serde_json::json!({"url": url, "title": c.title, "text": c.text, "markdown": c.markdown}),
                        format,
                    );
                }
                CrawlerCmd::Failures => {
                    let fails = crawlers.tracker().recent_failures(None);
                    print(&serde_json::json!({ "failures": fails }), format);
                }
                CrawlerCmd::Status => {
                    let list: Vec<_> = crawlers
                        .list()
                        .into_iter()
                        .map(|m| {
                            let status = crawlers.tracker().status(&m.id);
                            serde_json::json!({"id": m.id, "status": format!("{status:?}")})
                        })
                        .collect();
                    print(&serde_json::json!({ "crawlers": list }), format);
                }
                CrawlerCmd::Validate => {
                    let mut errors = Vec::new();
                    for m in crawlers.list() {
                        if let Err(e) = m.validate() {
                            errors.push(serde_json::json!({"id": m.id, "error": e}));
                        }
                    }
                    let ok = errors.is_empty();
                    print(&serde_json::json!({"valid": ok, "errors": errors}), format);
                    if !ok {
                        return Err(SearchOsError::Extension("crawler manifests invalid".into()));
                    }
                }
            }
            Ok(())
        }
        Command::Extension { cmd } => {
            let rt = runtime(cli_ref).await?;
            match cmd {
                ExtensionCmd::List { quarantined } => {
                    let records = if *quarantined {
                        rt.extensions.quarantined()
                    } else {
                        rt.extensions.list()
                    };
                    let list: Vec<_> = records
                        .into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "id": r.manifest.id,
                                "version": r.manifest.version,
                                "state": format!("{:?}", r.state),
                                "last_error": r.last_error,
                            })
                        })
                        .collect();
                    print(&serde_json::json!({ "extensions": list }), format);
                }
                ExtensionCmd::Validate => {
                    let mut errors = Vec::new();
                    for r in rt.extensions.list() {
                        if let Err(e) = r.manifest.validate() {
                            errors.push(serde_json::json!({"id": r.manifest.id, "error": e}));
                        }
                    }
                    let ok = errors.is_empty();
                    print(&serde_json::json!({"valid": ok, "errors": errors}), format);
                    if !ok {
                        return Err(SearchOsError::Extension(
                            "extension manifests invalid".into(),
                        ));
                    }
                }
                ExtensionCmd::Inspect { id } => {
                    let rec = rt.extensions.get(id).ok_or_else(|| {
                        SearchOsError::Extension(format!("extension {id} not found"))
                    })?;
                    print(
                        &serde_json::json!({
                            "id": rec.manifest.id,
                            "state": format!("{:?}", rec.state),
                            "manifest": rec.manifest,
                        }),
                        format,
                    );
                }
            }
            Ok(())
        }
        Command::Backup { cmd } => {
            // Restore is a pure file operation and must NOT initialise a
            // runtime: opening the SQLite database first would create an empty
            // database whose WAL is checkpointed over the restored snapshot
            // when the process exits — the exact bug the restore drill caught.
            // `create` needs a connection for the `VACUUM INTO` snapshot and
            // writes to a separate file, so it keeps the runtime.
            match cmd {
                BackupCmd::Create => {
                    let rt = runtime(cli_ref).await?;
                    let cfg = rt.config.clone();
                    let backup_dir = cfg.app.data_dir.join("data").join("backups");
                    // RFC-018 18.5: a consistent backup contains SQLite,
                    // redacted configuration and extension manifests, with
                    // content hashes in a manifest.
                    std::fs::create_dir_all(&backup_dir)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                    let dst = backup_dir.join(format!("searchos-{stamp}"));
                    std::fs::create_dir_all(&dst)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    // VACUUM INTO writes a consistent snapshot of the whole
                    // database, including transactions still sitting in the
                    // WAL. Copying the bare .db file would silently miss them
                    // (that is exactly what the restore drill caught).
                    let dst_db = dst.join("metadata.sqlite");
                    rt.sqlite.with_conn(|c| {
                        c.execute("VACUUM INTO ?1", [dst_db.to_string_lossy().as_ref()])
                            .map(|_| ())
                            .map_err(|e| {
                                SearchOsError::Storage(format!(
                                    "backup snapshot failed (VACUUM INTO): {e}"
                                ))
                            })
                    })?;
                    let db_bytes = std::fs::read(&dst_db)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let db_hash = searchos_core::hash_util::content_hash_hex(&db_bytes);
                    std::fs::write(dst.join("metadata.sqlite"), &db_bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let config_json = serde_json::to_string_pretty(&cfg.redacted())
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    std::fs::write(dst.join("config.json"), &config_json)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    // Extension manifests (declarative crawler rules).
                    let ext_dir = dst.join("extensions");
                    std::fs::create_dir_all(&ext_dir)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let mut ext_count = 0usize;
                    let crawlers_path = cfg.crawlers_path();
                    if let Ok(entries) = std::fs::read_dir(&crawlers_path) {
                        for entry in entries.flatten() {
                            if let Ok(files) = std::fs::read_dir(entry.path()) {
                                for f in files.flatten() {
                                    let p = f.path();
                                    if p.extension().and_then(|e| e.to_str()) == Some("yaml") {
                                        let name = format!(
                                            "{}-{}",
                                            entry.file_name().to_string_lossy(),
                                            p.file_name().unwrap_or_default().to_string_lossy()
                                        );
                                        let _ = std::fs::copy(&p, ext_dir.join(name));
                                        ext_count += 1;
                                    }
                                }
                            }
                        }
                    }
                    let manifest = serde_json::json!({
                        "format_version": 1,
                        "created_at": chrono::Utc::now().to_rfc3339(),
                        "sqlite": "metadata.sqlite",
                        "sqlite_sha256": db_hash,
                        "config": "config.json",
                        "config_sha256": searchos_core::hash_util::content_hash_hex(config_json.as_bytes()),
                        "extension_manifests": ext_count,
                        "indexes": "rebuildable from SQLite (ADR-022)",
                        "secrets": "[never included]",
                    });
                    std::fs::write(
                        dst.join("manifest.json"),
                        serde_json::to_string_pretty(&manifest)
                            .map_err(|e| SearchOsError::Internal(e.to_string()))?,
                    )
                    .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    print(
                        &serde_json::json!({
                            "backup": dst.to_string_lossy(),
                            "sqlite_snapshot": true,
                            "sqlite_sha256": db_hash,
                            "extension_manifests": ext_count,
                            "note": "consistent backup: SQLite + redacted config + extension manifests; Tantivy/LanceDB rebuildable (RFC-018 18.5)",
                        }),
                        format,
                    );
                }
                BackupCmd::Restore { path } => {
                    let cfg = load_config(cli_ref);
                    // RFC-018 18.6: verify hashes, DB integrity and schema
                    // compatibility before declaring success.
                    let (db_file, manifest_path) = if path.is_dir() {
                        (
                            path.join("metadata.sqlite"),
                            Some(path.join("manifest.json")),
                        )
                    } else {
                        (path.clone(), None)
                    };
                    let db_bytes = std::fs::read(&db_file)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let mut hash_verified = false;
                    if let Some(mp) = manifest_path {
                        let manifest: serde_json::Value = serde_json::from_str(
                            &std::fs::read_to_string(&mp)
                                .map_err(|e| SearchOsError::Storage(e.to_string()))?,
                        )
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                        if let Some(expected) = manifest["sqlite_sha256"].as_str() {
                            let actual = searchos_core::hash_util::content_hash_hex(&db_bytes);
                            if actual != expected {
                                return Err(SearchOsError::Storage(
                                    "backup checksum mismatch: refusing to restore".into(),
                                ));
                            }
                            hash_verified = true;
                        }
                    }
                    // Integrity check on the candidate before overwriting.
                    let tmp = std::env::temp_dir()
                        .join(format!("searchos-restore-check-{}.db", std::process::id()));
                    std::fs::write(&tmp, &db_bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let check_store = searchos_storage_sqlite::SqliteStore::open(&tmp)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let integrity: String = check_store
                        .with_conn(|c| {
                            c.query_row("PRAGMA integrity_check", [], |r| r.get(0))
                                .map_err(|e| SearchOsError::Storage(e.to_string()))
                        })
                        .unwrap_or_else(|_| "error".into());
                    let _ = std::fs::remove_file(&tmp);
                    if integrity != "ok" {
                        return Err(SearchOsError::Storage(format!(
                            "backup database integrity check failed: {integrity}"
                        )));
                    }
                    let target = cfg.data_db_path();
                    std::fs::write(&target, &db_bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    // A stale WAL beside the restored snapshot would be
                    // replayed over it on the next open, undoing the restore.
                    let stem = target.with_extension("");
                    for suffix in ["-wal", "-shm"] {
                        let _ = std::fs::remove_file(stem.with_extension(format!("db{suffix}")));
                    }
                    print(
                        &serde_json::json!({
                            "restored": target.to_string_lossy(),
                            "from": path.to_string_lossy(),
                            "hash_verified": hash_verified,
                            "integrity_check": integrity,
                            "verify": "run `searchos index verify` and `searchos index rebuild` after restore",
                        }),
                        format,
                    );
                }
            }
            Ok(())
        }
        Command::Mcp => {
            let rt = runtime(cli_ref).await?;
            searchos_interface_mcp::run_stdio(rt)
                .await
                .map_err(SearchOsError::Storage)?;
            Ok(())
        }
    }
}

fn parse_mode(s: &str) -> Result<SearchMode, SearchOsError> {
    SearchMode::parse(s).map_err(|message| SearchOsError::validation("mode", message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Every subcommand at every level must carry a description, so the
    /// `help` output never renders an empty `Commands:` line.
    #[test]
    fn every_subcommand_has_a_description() {
        fn assert_described(cmd: &mut clap::Command) {
            let parent = cmd.get_name().to_string();
            for sub in cmd.get_subcommands_mut() {
                let about = sub.get_about().map(|s| s.to_string()).unwrap_or_default();
                assert!(
                    !about.trim().is_empty(),
                    "subcommand '{}' of '{}' has no about",
                    sub.get_name(),
                    parent
                );
                assert_described(sub);
            }
        }
        assert_described(&mut Cli::command());
    }

    /// Every argument (positional or option) must carry help text.
    #[test]
    fn every_argument_has_help() {
        fn assert_args(cmd: &mut clap::Command) {
            for arg in cmd.get_arguments() {
                let help = arg.get_help().map(|s| s.to_string()).unwrap_or_default();
                assert!(
                    !help.trim().is_empty(),
                    "argument '{}' of '{}' has no help",
                    arg.get_id().as_str(),
                    cmd.get_name()
                );
            }
            for sub in cmd.get_subcommands_mut() {
                assert_args(sub);
            }
        }
        assert_args(&mut Cli::command());
    }

    /// Completion scripts must be generated for every supported shell and
    /// must reference the binary name.
    #[test]
    fn completions_generate_for_every_shell() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Elvish,
        ] {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "searchos", &mut out);
            let text = String::from_utf8(out).expect("utf-8 completion script");
            assert!(
                text.contains("searchos"),
                "{shell:?} completion missing binary name"
            );
        }
    }
}
