//! `searchos` CLI — Local Mode entry point (Part 18 command contract).
//!
//! Hard constraint (P-003): the CLI calls the in-process application. It
//! never round-trips through localhost HTTP; only `gateway start` binds a
//! socket (that is the Gateway Mode server itself).

mod output;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use output::{print, Format};
use searchos_core::{SearchOsConfig, SearchOsRuntime};
use searchos_domain::ids::{ProfileId, WorkspaceId};
use searchos_domain::memory::MemoryNamespace;
use searchos_domain::request::{SearchMode, SearchRequest};
use searchos_domain::SearchOsError;
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
enum CostCmd {
    Summary,
    Agents,
    Providers,
    Operations,
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
                    std::fs::create_dir_all(&backup_dir)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
                    let src = rt.config.data_db_path();
                    let dst = backup_dir.join(format!("searchos-{stamp}.db"));
                    let bytes =
                        std::fs::read(&src).map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    std::fs::write(&dst, bytes)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    print(
                        &serde_json::json!({
                            "backup": dst.to_string_lossy(),
                            "sqlite_snapshot": true,
                            "note": "Tantivy and LanceDB are rebuildable from SQLite (ADR-022)",
                        }),
                        format,
                    );
                }
                BackupCmd::Restore { path } => {
                    let target = rt.config.data_db_path();
                    std::fs::copy(path, &target)
                        .map_err(|e| SearchOsError::Storage(e.to_string()))?;
                    print(
                        &serde_json::json!({
                            "restored": target.to_string_lossy(),
                            "from": path.to_string_lossy(),
                            "verify": "run `searchos index verify` after restore",
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
