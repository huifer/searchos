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
use searchos_core::{SearchOsConfig, SearchOsRuntime};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::SearchOsError;
use searchos_evaluation::{runner::QueryResult as EvalQueryResult, EvaluationDataset};
use searchos_storage_sqlite::repos;

/// SearchOS — quality-first, cost-aware search infrastructure for agents.
#[derive(Parser)]
#[command(name = "searchos", version, about, long_about = None)]
struct Cli {
    /// Config file override (env: SEARCHOS_CONFIG)
    #[arg(long, global = true, env = "SEARCHOS_CONFIG")]
    config: Option<PathBuf>,
    /// Data directory override (env: SEARCHOS_DATA_DIR)
    #[arg(long, global = true, env = "SEARCHOS_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Output format
    #[arg(long, global = true, value_enum, default_value = "human")]
    format: Format,
    /// Non-interactive (never prompt)
    #[arg(long, global = true, default_value_t = true)]
    non_interactive: bool,
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
    /// Execute a search
    Search {
        query: String,
        #[arg(long, default_value = "general")]
        profile: String,
        /// local_only | local_first | external_first | external_only | hybrid
        #[arg(long, default_value = "local_first")]
        mode: String,
        #[arg(long, default_value = "default")]
        workspace: String,
        #[arg(long)]
        max_external_queries: Option<u32>,
        #[arg(long)]
        max_cost: Option<f64>,
        /// Client idempotency key (replaying it must not re-bill a search)
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Fetch and persist a URL
    Fetch {
        url: String,
        #[arg(long, default_value = "default")]
        workspace: String,
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
    /// Data export (RFC-024 24.5)
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
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
    List {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Inspect {
        id: String,
    },
    Refresh {
        id: String,
    },
    Delete {
        id: String,
    },
}

#[derive(Subcommand)]
enum MemoryCmd {
    Search {
        query: String,
        #[arg(long, default_value = "default")]
        workspace: String,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
    },
    Stats,
}

#[derive(Subcommand)]
enum ProviderCmd {
    List,
    Add { name: String },
    Health,
}

#[derive(Subcommand)]
enum ProfileCmd {
    List,
    Inspect { id: String },
}

#[derive(Subcommand)]
enum StatsCmd {
    Retrieval,
    Savings,
}

#[derive(Subcommand)]
enum ConfigCmd {
    Validate,
}

#[derive(Subcommand)]
enum EvalCmd {
    Run {
        /// Dataset JSONL path
        #[arg(long, default_value = "fixtures/evaluations/dataset-v1.jsonl")]
        dataset: PathBuf,
        /// Evaluate only the first N questions (0 = all)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CostCmd {
    Summary,
    Agents,
    Providers,
    Operations,
    Savings,
    Reservations,
    Reconcile {
        /// cost_events row id
        #[arg(long)]
        event: String,
        /// Actual billed amount in major units
        #[arg(long)]
        actual: f64,
        #[arg(long, default_value = "provider_billing")]
        source: String,
    },
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum TraceCmd {
    Show { trace_id: String },
}

#[derive(Subcommand)]
enum IndexCmd {
    Status,
    Stats,
    Rebuild,
    Verify,
}

#[derive(Subcommand)]
enum GatewayCmd {
    Start {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    Status,
}

#[derive(Subcommand)]
enum CrawlerCmd {
    List,
    Inspect { domain: String },
    Test { url: String },
    Preview { url: String },
    Failures,
    Status,
    Validate,
}

#[derive(Subcommand)]
enum ExtensionCmd {
    List,
    Validate,
    Inspect { id: String },
}

#[derive(Subcommand)]
enum BackupCmd {
    Create,
    Restore { path: PathBuf },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    searchos_observability::logging::init_with_default("warn");
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            match &e {
                SearchOsError::Validation(_) => 2,
                SearchOsError::Budget(_) => 3,
                SearchOsError::Policy(_) | SearchOsError::Security(_) => 4,
                SearchOsError::Provider(_)
                | SearchOsError::Retrieval(_)
                | SearchOsError::Timeout(_) => 5,
                _ => 1,
            }
        }
    };
    std::process::exit(code);
}

fn load_config(cli: &Cli) -> SearchOsConfig {
    let mut cfg = searchos_core::load_config(cli.config.as_deref());
    if let Some(dir) = &cli.data_dir {
        cfg.app.data_dir = dir.clone();
    }
    cfg
}

async fn runtime(cli: &Cli) -> Result<Arc<SearchOsRuntime>, SearchOsError> {
    let cfg = load_config(cli);
    SearchOsRuntime::init(cfg).await
}

async fn run(cli: Cli) -> Result<(), SearchOsError> {
    let format = cli.format;
    // Pre-load config before the match moves subcommand fields.
    let cli = &cli;
    let config_now = load_config(cli);
    let config_for_later = config_now.clone();
    let _ = config_for_later;
    let cli_owned = cli;
    let _ = cli_owned;
    // Destructure the command by reference to avoid partial moves.
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
        Command::Search {
            query,
            profile,
            mode,
            workspace,
            max_external_queries,
            max_cost,
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
            let resp = rt.application.search_service.search(req).await?;
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
        Command::Document { cmd } => {
            let rt = runtime(cli_ref).await?;
            let repo = repos::DocumentRepo::new(&rt.sqlite);
            match cmd {
                DocumentCmd::List { limit } => {
                    let docs = repo.list(*limit)?;
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
                    let refreshed = rt
                        .application
                        .fetch_service
                        .fetch_and_persist(&doc.canonical_url, &WorkspaceId::new("default"))
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
                        out.push(serde_json::json!({
                            "provider_id": id,
                            "state": format!("{state:?}"),
                            "rate_limited": rate_limited,
                        }));
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
                let (cfg, errors) = searchos_core::load_config_report(cli.config.as_deref());
                let valid = errors.is_empty();
                print(
                    &serde_json::json!({
                        "valid": valid,
                        "config_version": cfg.config_version,
                        "api_version": cfg.server.api_version,
                        "errors": errors,
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
                "index": { "tantivy_chunks": tantivy_chunks, "sqlite_chunks": sqlite_chunks },
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
                    let (tantivy, sqlite) = rt.index_consistency().await?;
                    print(
                        &serde_json::json!({
                            "tantivy_version": rt.tantivy.version(),
                            "tantivy_chunks": tantivy,
                            "sqlite_chunks": sqlite,
                            "consistent": tantivy == sqlite,
                        }),
                        format,
                    );
                }
                IndexCmd::Stats => {
                    // Alias of `index status` (MVP §8.4).
                    let (tantivy, sqlite) = rt.index_consistency().await?;
                    print(
                        &serde_json::json!({
                            "tantivy_version": rt.tantivy.version(),
                            "tantivy_chunks": tantivy,
                            "sqlite_chunks": sqlite,
                            "consistent": tantivy == sqlite,
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
                ExtensionCmd::List => {
                    let list: Vec<_> = rt.extensions.list().into_iter().map(|r| serde_json::json!({
                        "id": r.manifest.id, "version": r.manifest.version, "state": format!("{:?}", r.state),
                    })).collect();
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
            let rt = runtime(cli_ref).await?;
            let backup_dir = rt.config.app.data_dir.join("data").join("backups");
            match cmd {
                BackupCmd::Create => {
                    // RFC-018 18.5: a consistent backup contains SQLite,
                    // redacted configuration and extension manifests, with
                    // content hashes in a manifest.
                    std::fs::create_dir_all(&backup_dir)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                    let dst = backup_dir.join(format!("searchos-{stamp}"));
                    std::fs::create_dir_all(&dst)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let src = rt.config.data_db_path();
                    let db_bytes =
                        std::fs::read(&src).map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let db_hash = searchos_core::hash_util::content_hash_hex(&db_bytes);
                    std::fs::write(dst.join("metadata.sqlite"), &db_bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let config_json = serde_json::to_string_pretty(&rt.config.redacted())
                        .map_err(|e| SearchOsError::Internal(e.to_string()))?;
                    std::fs::write(dst.join("config.json"), &config_json)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    // Extension manifests (declarative crawler rules).
                    let ext_dir = dst.join("extensions");
                    std::fs::create_dir_all(&ext_dir)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let mut ext_count = 0usize;
                    let crawlers_path = rt.config.crawlers_path();
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
                    let target = rt.config.data_db_path();
                    std::fs::write(&target, &db_bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
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
    Ok(match s {
        "local_only" | "local-only" => SearchMode::LocalOnly,
        "local_first" | "local-first" => SearchMode::LocalFirst,
        "external_first" | "external-first" => SearchMode::ExternalFirst,
        "external_only" | "external-only" => SearchMode::ExternalOnly,
        "hybrid" => SearchMode::Hybrid,
        other => {
            return Err(SearchOsError::validation(
                "mode",
                format!("unknown mode {other}; use local_only|local_first|external_first|external_only|hybrid"),
            ))
        }
    })
}
