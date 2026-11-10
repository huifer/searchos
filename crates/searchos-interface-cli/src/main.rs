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
        /// Cap on total estimated cost for this request (major units)
        #[arg(long)]
        max_cost: Option<f64>,
        /// Maximum number of results returned after the quality gate
        #[arg(long)]
        max_results: Option<u32>,
        /// Client idempotency key (replaying it must not re-bill a search)
        #[arg(long)]
        request_id: Option<String>,
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
    /// Cost ledger operations
    Cost {
        #[command(subcommand)]
        cmd: CostCmd,
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
            max_cost,
            max_results,
            request_id,
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
            if let Some(c) = max_cost {
                req.budget.max_total_cost_micros = (c * 1_000_000.0).round() as i64;
            }
            if let Some(rid) = request_id {
                req = req.with_request_id(rid.clone());
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
                    repo.delete(&searchos_domain::ids::DocumentId::new(id.clone()))?;
                    print(&serde_json::json!({ "deleted": id }), format);
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
