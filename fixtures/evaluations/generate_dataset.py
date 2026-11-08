#!/usr/bin/env python3
"""Generates the RFC-015 15.3 first-stage evaluation dataset.

Category targets (RFC-015 15.3):
  静态技术知识        100
  技术文档            100
  GitHub               75
  新闻与最新信息        75
  产品及 API 价格       50
  复杂对比研究          50
  中英文混合              50
Total: 500 questions.

The dataset is committed as dataset-v1.jsonl so runs are reproducible.
Questions are templated from entity lists; deeper human labeling per
RFC-015 15.6 happens on later calibration rounds.
"""
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).parent

CONCEPTS = [
    "Rust", "Tokio", "Axum", "Serde", "Tantivy", "SQLite", "Raft", "CAP theorem",
    "gRPC", "Protobuf", "Kubernetes", "Docker", "Git", "LLVM", "WASM", "HTTP/2",
    "QUIC", "TLS", "JWT", "OAuth", "CRDT", "Bloom filter", "LSM tree", "B-tree",
    "MMAP", "Zero-copy", "Async runtime", "Actor model", "Backpressure", "Circuit breaker",
    "Consistent hashing", "Vector embedding", "BM25", "TF-IDF", "Reciprocal Rank Fusion",
    "Cross encoder", "Reranking", "Semantic cache", "Content hash", "Merkle tree",
    "Transactional outbox", "Event sourcing", "CQRS", "Saga pattern", "Idempotency",
    "Rate limiting", "Token bucket", "Leaky bucket", "Singleflight", "SSRF",
    "Prompt injection", "DNS rebinding", "CORS", "CSP", "HSTS", "SSO", "RBAC",
    "ACID", "MVCC", "WAL", "Two-phase commit", "Paxos",
    "Rust ownership", "Rust borrowing", "Rust lifetimes", "Rust traits",
    "Rust async/await", "Rust macros", "Rust unsafe", "Rust closures",
    "Trait objects", "Generics", "Monomorphization", "Vtable dispatch",
    "Memory alignment", "Cache line", "SIMD", "Branch prediction", "Instruction pipelining",
    "Speculative execution", "Out-of-order execution", "Memory barriers", "Atomics",
    "Lock-free programming", "Memory ordering", "False sharing", "NUMA",
    "Page cache", "Copy-on-write", "Journaling filesystem", "FUSE",
    "HTTP caching", "CDN", "Load balancing", "Service mesh", "Sidecar proxy",
    "Observability", "Distributed tracing", "OpenTelemetry", "Error budget",
]
assert len(CONCEPTS) == 100, len(CONCEPTS)

DOC_ENTITIES = [
    ("tokio", "Tokio"), ("axum", "Axum"), ("serde", "Serde"), ("reqwest", "Reqwest"),
    ("clap", "Clap"), ("rusqlite", "Rusqlite"), ("tantivy", "Tantivy"), ("tracing", "Tracing"),
    ("hyper", "Hyper"), ("tower", "Tower"), ("tonic", "Tonic"), ("sqlx", "SQLx"),
    ("uuid", "Uuid"), ("chrono", "Chrono"), ("rand", "Rand"), ("regex", "Regex"),
    ("walkdir", "WalkDir"), ("anyhow", "Anyhow"), ("thiserror", "ThisError"),
    ("futures", "Futures"), ("bytes", "Bytes"), ("parking_lot", "ParkingLot"),
    ("dashmap", "DashMap"), ("crossbeam", "Crossbeam"), ("rayon", "Rayon"),
    ("polars", "Polars"), ("ndarray", "Ndarray"), ("image", "Image"),
    ("scraper", "Scraper"), ("html5ever", "Html5ever"), ("quick-xml", "Quick-XML"),
    ("yaml", "serde-yaml"), ("json5", "json5"), ("toml", "toml"), ("csv", "csv"),
    ("itertools", "Itertools"), ("once_cell", "once_cell"), ("lazy_static", "lazy_static"),
    ("async-trait", "async-trait"), ("async-channel", "async-channel"),
    ("smol", "Smol"), ("actix-web", "Actix Web"), ("warp", "Warp"), ("rocket", "Rocket"),
    ("salsa", "Salsa"), ("petgraph", "Petgraph"), ("indexmap", "IndexMap"),
    ("ahash", "AHash"), ("base64", "Base64"), ("sha2", "SHA-2 crate"),
    ("hmac", "HMAC crate"), ("aes-gcm", "AES-GCM crate"), ("ring", "Ring"),
    ("rustls", "Rustls"), ("native-tls", "Native TLS"), ("quinn", "Quinn"),
    ("yoke", "Yoke"), ("zerocopy", "Zerocopy"), ("smallvec", "SmallVec"),
    ("arrayvec", "ArrayVec"), ("bitflags", "BitFlags"), ("num_cpus", "num_cpus"),
    ("tempfile", "TempFile"), ("memmap2", "Memmap2"), ("fs_extra", "fs_extra"),
    ("notify", "Notify"), ("indicatif", "Indicatif"), ("console", "Console"),
    ("dialoguer", "Dialoguer"), ("crossterm", "Crossterm"), ("ratatui", "Ratatui"),
    ("tui", "Tui-rs"), ("arboard", "Arboard"), ("open", "Open crate"),
    ("dirs", "Dirs"), ("home", "Home crate"), ("which", "Which crate"),
    ("glob", "Glob crate"), ("walkdir-crate2", "WalkDir variants"),
    ("similar", "Similar crate"), ("diffs", "Diff crate"), ("insta", "Insta"),
    ("proptest", "Proptest"), ("quickcheck", "QuickCheck"), ("mockall", "Mockall"),
    ("wiremock", "Wiremock"), ("assert_cmd", "assert_cmd"), ("predicates", "Predicates"),
    ("criterion", "Criterion"), ("iai", "Iai"), ("divan", "Divan"),
    ("pprof", "pprof-rs"), ("flamegraph", "Flamegraph"), ("valgrind", "Valgrind"),
    ("perf", "Perf"), ("dhat", "dhat"), ("mimalloc", "Mimalloc"),
    ("jemalloc", "Jemalloc"), ("tikv-jemalloc", "TiKV Jemalloc"),
    ("snmalloc", "SnMalloc"),
]
assert len(DOC_ENTITIES) == 100, len(DOC_ENTITIES)

GITHUB_REPOS = [
    "tokio-rs/tokio", "hyperium/hyper", "rust-lang/rust", "rust-lang/cargo",
    "clap-rs/clap", "serde-rs/serde", "seanmonstar/reqwest", "launchbadge/sqlx",
    "quickwit-oss/tantivy", "tokio-rs/axum", "actix/actix-web", "seanmonstar/warp",
    "SergioBenitez/Rocket", "diesel-rs/diesel", "risingwavelabs/risingwave",
    "databendlabs/openraft", "tikv/tikv", "pingcap/tidb", "cockroachdb/cockroach",
    "torvalds/linux", "clickhouse/clickhouse", "duckdb/duckdb", "apache/arrow",
    "delta-io/delta", "apache/datafusion", "polars-rs/polars", "lancedb/lance",
    "lancedb/lancedb", "qdrant/qdrant", "weaviate/weaviate", "milvus-io/milvus",
    "chroma-core/chroma", "pgvector/pgvector", "supabase/supabase",
    "neondatabase/neon", "vitessio/vitess", "trinodb/trino", "prestodb/presto",
    "apache/spark", "apache/flink", "apache/kafka", "redpanda-data/redpanda",
    "nats-io/nats-server", "rabbitmq/rabbitmq-server", "zeromq/libzmq",
    "nanomsg/nng", "grpc/grpc", "envoyproxy/envoy", "istio/istio",
    "linkerd/linkerd2", "hashicorp/consul", "hashicorp/nomad", "hashicorp/vault",
    "kubernetes/kubernetes", "containerd/containerd", "moby/moby",
    "docker/compose", "helm/helm", "argoproj/argo-cd", "tektoncd/pipeline",
    "fluxcd/flux2", "crossplane/crossplane", "prometheus/prometheus",
    "grafana/grafana", "jaegertracing/jaeger", "open-telemetry/opentelemetry-rust",
    "open-telemetry/opentelemetry-go", "vectordotdev/vector", "fluent/fluentd",
    "rsyslog/rsyslog", "grafana/loki", "elastic/logstash", "pingcap/ticdc",
    "debezium/debezium", "confluentinc/ksql", "airbytehq/airbyte",
    "dbt-labs/dbt-core", "great-expectations/great_expectations",
    "singer-io/tap-github", "missive/pgmq", "pgbouncer/pgbouncer",
    "postgres/postgres", "mysql/mysql-server", "mariadb/server", "redis/redis",
    "memcached/memcached", "etcd-io/etcd", "apache/zookeeper",
    "elastic/elasticsearch", "apache/pulsar", "ceph/ceph", "minio/minio",
    "apache/hadoop", "alluxio/alluxio", "apache/hbase", "cassandra/cassandra",
    "scylladb/scylladb", "influxdata/influxdb", "questdb/questdb",
    "timescale/timescaledb",
]
assert len(GITHUB_REPOS) == 100, len(GITHUB_REPOS)

NEWS_TOPICS = [
    "AI regulation", "chip export controls", "Rust foundation", "LLM benchmarks",
    "developer survey", "cloud pricing change", "browser release", "OS kernel LTS",
    "database funding round", "vector DB acquisition", "security CVE disclosure",
    "open source license change", "Kubernetes release", "HTTP standard update",
    "crypto exchange incident", "quantum computing milestone", "RISC-V adoption",
    "ARM server chips", "data center power", "green computing",
    "AI assistant enterprise rollout", "code assistant adoption",
    "API deprecation notice", "framework EOL announcement", "supply chain attack",
    "container registry outage", "DNS provider incident", "CDN failure analysis",
    "edge computing expansion", "5G deployment", "satellite internet",
    "battery technology", "EV charging standard", "smart home standard",
    "robotics deployment", "self-driving progress", "medical AI approval",
    "protein folding result", "fusion energy record", "space launch schedule",
    "lunar mission update", "mars sample return", "telescope discovery",
    "exoplanet atmosphere", "supercomputer ranking", "HPC interconnect",
    "storage price trend", "SSD endurance study", "memory bandwidth record",
    "networking throughput", "optical interconnect", "photonic computing",
    "neuromorphic chip", "analog AI accelerator", "compiler optimization win",
    "language runtime update", "garbage collector improvement", "JIT startup time",
    "wasm runtime perf", "emulator accuracy", "retro computing revival",
    "open hardware laptop", "single board computer", "microcontroller release",
    "sensor fusion paper", "localization dataset", "SLAM benchmark",
    "simulation engine release", "physics engine perf", "rendering technique",
    "graphics API update", "GPU driver release", "upscaler model release",
    "speech model release", "translation quality gain", "OCR accuracy record",
    "recommendation paper", "ranking model update", "ad auction study",
    "marketplace economics", "creator economy report", "developer salary survey",
    "remote work trend", "open source sustainability", "maintainer burnout study",
    "dependency audit tool", "SBOM adoption", "provenance attestation",
    "reproducible build effort", "supply chain signing", "key transparency log",
    "certificate authority incident", "domain hijack report", "registrar outage",
    "BGP incident analysis", "route leak postmortem", "anycast capacity test",
    "undersea cable repair", "internet exchange growth", "rural broadband subsidy",
]
assert len(NEWS_TOPICS) == 100, len(NEWS_TOPICS)

PRICING_ENTITIES = [
    ("OpenAI", "GPT-4o"), ("OpenAI", "o3"), ("Anthropic", "Claude"),
    ("Google", "Gemini"), ("AWS", "Lambda"), ("AWS", "S3"), ("AWS", "EC2"),
    ("GCP", "Cloud Run"), ("GCP", "BigQuery"), ("Azure", "Functions"),
    ("Azure", "Blob Storage"), ("Cloudflare", "Workers"), ("Cloudflare", "R2"),
    ("Vercel", "Serverless Functions"), ("Netlify", "Functions"),
    ("Supabase", "Database"), ("Neon", "Serverless Postgres"), ("PlanetScale", "Vitess"),
    ("Planetscale", "Scaling"), ("MongoDB", "Atlas"), ("Redis", "Cloud"),
    ("Elastic", "Elasticsearch"), ("Algolia", "Search"), ("Typesense", "Cloud"),
    ("Meilisearch", "Cloud"), ("Pinecone", "Vector DB"), ("Weaviate", "Cloud"),
    ("Qdrant", "Cloud"), ("Zilliz", "Managed Milvus"), ("DataDog", "APM"),
    ("NewRelic", "APM"), ("Honeycomb", "Events"), ("Grafana", "Cloud"),
    ("Sentry", "Errors"), ("PagerDuty", "Incidents"), ("Twilio", "SMS"),
    ("SendGrid", "Email"), ("Postmark", "Email"), ("Resend", "Email"),
    ("Stripe", "Payments"), ("Adyen", "Payments"), ("PayPal", "Checkout"),
    ("Shopify", "Commerce"), ("Auth0", "Identity"), ("Okta", "Identity"),
    ("Clerk", "Auth"), ("Supabase", "Auth"), ("Firebase", "Auth"),
    ("Mapbox", "Maps"), ("Google", "Maps API"),
]
assert len(PRICING_ENTITIES) == 50, len(PRICING_ENTITIES)

COMPARISONS = [
    ("Tokio", "Axum"), ("Tokio", "async-std"), ("Actix Web", "Axum"),
    ("Warp", "Axum"), ("Rocket", "Actix Web"), ("SQLx", "Diesel"),
    ("Rusqlite", "SQLx"), ("Postgres", "MySQL"), ("SQLite", "Postgres"),
    ("Redis", "Memcached"), ("Kafka", "RabbitMQ"), ("NATS", "Kafka"),
    ("gRPC", "REST"), ("GraphQL", "REST"), ("Tantivy", "Lucene"),
    ("Elasticsearch", "OpenSearch"), ("Qdrant", "Weaviate"), ("Milvus", "Qdrant"),
    ("Pinecone", "Qdrant"), ("LanceDB", "Qdrant"), ("Consul", "Etcd"),
    ("Kubernetes", "Nomad"), ("Docker", "Podman"), ("Terraform", "Pulumi"),
    ("Datadog", "Grafana"), ("Sentry", "Rollbar"), ("S3", "R2"),
    ("BigQuery", "Snowflake"), ("ClickHouse", "DuckDB"), ("DuckDB", "Polars"),
    ("Polars", "Pandas"), ("Ray", "Dask"), ("Spark", "Flink"),
    ("Kubernetes", "Swarm"), ("Istio", "Linkerd"), ("Envoy", "NGINX"),
    ("Caddy", "NGINX"), ("Traefik", "Caddy"), ("Cloudflare Workers", "Lambda@Edge"),
    ("Vercel", "Netlify"), ("Supabase", "Firebase"), ("Auth0", "Clerk"),
    ("Stripe", "Adyen"), ("Rust", "Go"), ("Rust", "C++"), ("Go", "Java"),
    ("TypeScript", "Python"), ("Python", "Node.js"), ("WASM", "Native"),
    ("Rust", "Zig"),
]
assert len(COMPARISONS) == 50, len(COMPARISONS)

ZH_EN = [
    ("Rust 和 Go 哪个更适合 Web 后端开发", ["rust", "go"]),
    ("Tokio 异步运行时是什么", ["tokio", "runtime"]),
    ("最新版本的 Rust 是多少", ["rust"]),
    ("SQLite 和 PostgreSQL 的区别", ["sqlite", "postgres"]),
    ("Kubernetes 是怎么调度的", ["kubernetes"]),
    ("BM25 和向量检索如何融合", ["bm25"]),
    ("Tantivy 全文索引怎么用", ["tantivy"]),
    ("LLM 的上下文窗口是什么意思", ["llm"]),
    ("Axum 路由怎么定义", ["axum"]),
    ("向量数据库有哪些选择", ["vector"]),
    ("Rust 所有权机制是什么", ["ownership"]),
    ("怎么排查 DNS 解析失败", ["dns"]),
    ("现在最新的 Claude 模型是什么", ["claude"]),
    ("OpenAI API 的价格是多少", ["openai", "price"]),
    ("Raft 共识算法怎么工作", ["raft"]),
    ("Docker 和 Podman 有什么区别", ["docker", "podman"]),
    ("gRPC 和 REST 该怎么选", ["grpc"]),
    ("SSRF 攻击怎么防御", ["ssrf"]),
    ("Tokio 和 async-std 性能对比", ["tokio"]),
    ("Redis 持久化 RDB 和 AOF 的区别", ["redis"]),
    ("Git rebase 和 merge 的区别", ["git"]),
    ("现在的 LLM 推理成本怎么算", ["inference"]),
    ("Kubernetes Ingress 怎么配置", ["ingress"]),
    ("HTTP/3 和 QUIC 是什么关系", ["quic"]),
    ("分布式追踪怎么实现", ["tracing"]),
    ("Rust 的 async fn 是怎么实现的", ["async"]),
    ("怎么做数据库备份恢复", ["backup"]),
    ("最新的 Node.js 版本", ["node"]),
    ("Cloudflare Workers 免费额度", ["cloudflare"]),
    ("Postgres 的 MVCC 是什么", ["mvcc"]),
    ("共识算法 Paxos 和 Raft 对比", ["raft"]),
    ("怎么做 API 限流", ["rate"]),
    ("Git 大文件管理用什么", ["lfs"]),
    ("KV 缓存和 CDN 缓存的区别", ["cache"]),
    ("Rust 和 Python 互操作怎么写", ["pyo3"]),
    ("矩阵乘法优化有哪些方法", ["simd"]),
    ("现在主流的向量检索算法", ["hnsw"]),
    ("Kafka 的 ISR 是什么", ["kafka"]),
    ("LLM 微调和 RAG 怎么选", ["rag"]),
    ("怎么压测一个 HTTP 服务", ["benchmark"]),
    ("现在的 AI 编程助手有哪些", ["assistant"]),
    ("WebAssembly 能替代 Docker 吗", ["wasm"]),
    ("GraphQL 的 N+1 问题怎么解决", ["graphql"]),
    ("数据库连接池怎么调参", ["pool"]),
    ("OAuth2 的几种授权流程", ["oauth"]),
    ("TLS 证书轮换怎么做", ["tls"]),
    ("消息队列的幂等消费怎么做", ["idempotent"]),
    ("容器镜像怎么瘦身", ["image"]),
    ("服务网格值得引入吗", ["mesh"]),
    ("现在的云数据库哪家便宜", ["cloud"]),
]
assert len(ZH_EN) == 50, len(ZH_EN)


def w(fh, qid, q, cat, keywords, time_sensitive=False, profile=None):
    obj = {
        "id": qid,
        "question": q,
        "category": cat,
        "expected_keywords": keywords,
        "time_sensitive": time_sensitive,
    }
    if profile:
        obj["profile"] = profile
    fh.write(json.dumps(obj, ensure_ascii=False) + "\n")


def main():
    out = HERE / "dataset-v1.jsonl"
    n = 0
    with out.open("w", encoding="utf-8") as fh:
        # 100 static tech knowledge
        for i, c in enumerate(CONCEPTS):
            n += 1
            w(fh, f"static-{i:03d}", f"What is {c}?", "static_tech_knowledge",
              [c.split()[0].lower()])
        # 100 tech documentation
        for i, (crate, name) in enumerate(DOC_ENTITIES):
            n += 1
            w(fh, f"docs-{i:03d}", f"Where is the official documentation for the {name} crate and what does it cover?",
              "tech_documentation", [name.lower().replace(" ", ""), "documentation"])
        # 75 GitHub
        for i, repo in enumerate(GITHUB_REPOS[:75]):
            n += 1
            w(fh, f"github-{i:03d}", f"What is the latest release of {repo} on GitHub?",
              "github", [repo.split("/")[1].lower()], time_sensitive=True, profile="latest")
        # 75 news / latest info
        for i, t in enumerate(NEWS_TOPICS[:75]):
            n += 1
            w(fh, f"news-{i:03d}", f"What is the latest news about {t}?",
              "news_latest", [t.split()[0].lower()], time_sensitive=True, profile="latest")
        # 50 pricing
        for i, (vendor, product) in enumerate(PRICING_ENTITIES):
            n += 1
            w(fh, f"price-{i:03d}", f"What is the current pricing of the {vendor} {product} API?",
              "product_api_pricing", [vendor.lower().replace(" ", ""), "pricing"],
              time_sensitive=True)
        # 50 comparisons
        for i, (a, b) in enumerate(COMPARISONS):
            n += 1
            w(fh, f"compare-{i:03d}", f"Compare {a} and {b}: features, maturity and latest version.",
              "complex_comparison", [a.lower(), b.lower()], profile="research")
        # 50 mixed zh/en
        for i, (q, kws) in enumerate(ZH_EN):
            n += 1
            w(fh, f"zh-{i:03d}", q, "mixed_zh_en", kws)
    assert n == 500, n
    print(f"wrote {n} questions to {out}")


if __name__ == "__main__":
    sys.exit(main())
