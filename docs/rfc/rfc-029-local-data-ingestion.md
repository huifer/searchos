# RFC-029: Local Data Ingestion & Local-First Retrieval

> 本地数据导入与本地优先检索

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline
**Related:** RFC-006 (Document Lifecycle), RFC-007 (Retrieval), RFC-010 (Fetch &
Crawler), RFC-012 (Runtime Modes), RFC-013 (Memory), RFC-014 (Security),
ADR-022 (Index Rebuildability), ADR-023 (Workspace Isolation), ADR-028 (Local
Ingestion).

---

## English

### 1. Context

SearchOS is local-first: the decision engine evaluates local knowledge before
spending on external providers, and `mode=local_only` forbids every external
call. But the only ingestion path shipped so far is **URL fetch**
(`POST /extract`, `POST /documents`, CLI `searchos fetch`). There is no way to
put a user's own material — a pasted note, a Markdown file, a CSV export, a
directory of documents — into the same knowledge store that `local_only` and
`local_first` already query.

This RFC closes that gap. Local ingestion must not become a parallel pipeline:
it has to feed the **same** document lifecycle (RFC-006), the **same**
transactional outbox (RFC-006 6.7), the **same** `SearchIndex` port (RFC-002
2.4) and the **same** `LocalKnowledgePort` (RFC-004) that URL-fetched documents
use. Otherwise local data would be a second-class citizen in retrieval, quality
gating and rebuild.

### 2. Goals

| ID | Goal |
|----|------|
| G1 | Ingest raw text, single files and directories into the knowledge store. |
| G2 | Reuse the existing document/version/chunk/outbox path — no duplicate indexing logic. |
| G3 | Imported content is retrievable under `local_only`, `local_first` and `hybrid` exactly like fetched content. |
| G4 | Imported content is **not** treated as stale purely because of its age; the user owns it. |
| G5 | Safe ingestion: explicit size bounds, content-type sniffing, no server-side trust of client filenames. |
| G6 | Both runtime modes: CLI (`Local Mode`, in-process) and HTTP Gateway. |
| G7 | Deduplication and versioning semantics that mirror URL fetch: same logical source + same content ⇒ no-op; changed content ⇒ new version. |

### 3. Non-goals

| ID | Non-goal |
|----|----------|
| N1 | Parsing binary office formats (PDF / DOCX / PPTX) in core v1. An extension point is defined; the first party extractors ship later. |
| N2 | Filesystem watching, automatic re-sync, or two-way write-back to source files. |
| N3 | OCR, audio or video extraction. |
| N4 | Secret / PII scanning of imported content. Imported content is user-trusted data. |
| N5 | Distributed or multi-node ingestion. |
| N6 | Changing the `local_only` / `local_first` decision semantics. They already do the right thing once data is indexed. |

### 4. Architecture

```text
   CLI: searchos ingest --text | --file | --dir
   HTTP: POST /ingest | /ingest/file | /ingest/batch
                    │
                    ▼
        ┌───────────────────────────┐
        │   IngestService (app)     │
        │  validate → extract →     │
        │  content-hash → build     │
        │  Document/Version/Chunks  │
        └─────────────┬─────────────┘
                      │  same DocumentPort
                      ▼
        ┌───────────────────────────┐
        │ SQLite + Object Store     │
        │ documents / versions /    │
        │ chunks / index_outbox     │
        └─────────────┬─────────────┘
                      │  outbox worker (already exists)
                      ▼
        ┌───────────────────────────┐
        │ SearchIndex port          │
        │ Tantivy | ES | Meili | PG │
        │ | MySQL (ADR-027)         │
        └─────────────┬─────────────┘
                      │
                      ▼
        ┌───────────────────────────┐
        │ LocalKnowledgePort        │
        │ → DecisionEngine          │
        │ (local_candidates)        │
        └───────────────────────────┘
```

`IngestService` is a **sibling of `FetchService`**, not a replacement. Both
call `DocumentPort::save_versioned`, so versioning, chunking, outbox events,
index rebuild and workspace isolation are inherited for free.

### 5. Domain Model

#### 5.1 Source identity

Local content has no URL. SearchOS assigns a **canonical local URI**:

```text
searchos-local://<workspace_id>/<source_key>
```

- `source_key` is the logical identity of the source.
- If the caller supplies `source_key`, it is normalized (trimmed, lower-cased,
  path separators collapsed to `/`, leading `/` removed) and used verbatim.
- If the caller does not supply one, SearchOS derives
  `source_key = sha256(content)[:16]`, making the source **content-addressed**.
  Identical content then dedupes to one document; edited content becomes a new
  document unless the caller pinned a stable key.

This mirrors URL fetch: the canonical URL is the identity, the content hash is
the version discriminator.

#### 5.2 Document fields

`Document` gains two fields (backward compatible, serde defaults):

```rust
pub struct Document {
    // ... existing fields ...
    /// "web" | "local_text" | "local_file" | "local_directory"
    pub source_type: String,
    /// Human title (file name, document title, or caller-supplied).
    pub title: Option<String>,
}
```

`source_type` is written to the existing `sources.source_type` column instead
of the currently hard-coded `"web"`. `domain` for a local source is the fixed
string `"local"`; for a web source it remains `domain_of(canonical_url)`.

#### 5.3 Ingest request

```rust
pub struct IngestRequest {
    pub source_key: Option<String>,
    pub title: Option<String>,
    pub content_type: Option<String>,   // hint; sniffed when absent
    pub language: Option<String>,
    pub workspace_id: WorkspaceId,
    pub metadata: serde_json::Value,    // caller labels, never queried by core
}

pub enum IngestContent {
    Text(String),
    File { path: PathBuf, file_name: String },
    Batch(Vec<(IngestRequest, IngestContent)>),
}
```

#### 5.4 Freshness category

A new `ContentCategory::Local` is added with `default_ttl_seconds() = None`
(never age-stale). `FreshnessPolicy::is_fresh` returns `true` for local
candidates unless the caller explicitly overrides the TTL in
`ttl_overrides["local"]`. This is required so a user's own notes are not
re-fetched or flagged stale after a year.

### 6. Interface Contract

#### 6.1 HTTP (Gateway Mode)

| Method | Path | Body | Notes |
|--------|------|------|-------|
| `POST` | `/ingest` | JSON `IngestRequest` + `content` string | Text ingest |
| `POST` | `/ingest/file` | `multipart/form-data`: `file` + metadata fields | File upload |
| `POST` | `/ingest/batch` | JSON array of `{request, content}` | Directory / bulk |
| `GET` | `/ingest/jobs/:id` | — | Batch job status |

Response envelope (matches the existing gateway style):

```json
{
  "document_id": "doc_...",
  "version_id": "docv_...",
  "canonical_url": "searchos-local://default/notes/alpha",
  "source_type": "local_file",
  "content_hash": "sha256:...",
  "changed": true,
  "title": "alpha.md",
  "chunk_count": 12,
  "status": "indexed"
}
```

Errors: `EMPTY_CONTENT`, `CONTENT_TOO_LARGE`, `INVALID_ENCODING`,
`UNSUPPORTED_CONTENT_TYPE`, `SOURCE_KEY_CONFLICT`, `WORKSPACE_DENIED`.
All handlers authenticate and enforce `authorize_workspace` exactly like
`/extract`.

#### 6.2 CLI (Local Mode, in-process)

```bash
searchos ingest --text "..." [--title "..."] [--workspace ws] [--source-key k]
searchos ingest --file ./notes.md [--source-key notes/alpha] [--workspace ws]
searchos ingest --dir ./docs --recursive [--ext md,txt] [--workspace ws]
searchos ingest --batch ./jobs.json
```

`searchos document list` gains `--source-type web|local` and `--workspace`.
`searchos document inspect` already works unchanged.

#### 6.3 MCP

MCP exposes RFC-021's seven tools plus the documented `search_trace`
compatibility alias, and now also exposes `ingest_text` / `ingest_file`
(RFC-029 follow-up amendment). The port is in-process-callable from MCP
handlers; text and in-memory file bytes are accepted, while a server-side
filesystem path is never accepted from MCP.

### 7. Data Model

Migration **v4** (`local_ingestion`) appends:

```sql
-- Document provenance: local vs web, and a human title.
ALTER TABLE documents ADD COLUMN source_type TEXT NOT NULL DEFAULT 'web';
ALTER TABLE documents ADD COLUMN title TEXT;

-- Batch / directory ingest jobs (observability + resumability).
CREATE TABLE IF NOT EXISTS ingest_jobs (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    source_type TEXT NOT NULL,          -- local_text | local_file | local_directory
    state TEXT NOT NULL,                -- pending | running | completed | partial | failed
    total_items INTEGER NOT NULL DEFAULT 0,
    ok_items INTEGER NOT NULL DEFAULT 0,
    failed_items INTEGER NOT NULL DEFAULT 0,
    error_detail TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_ingest_jobs_ws ON ingest_jobs(workspace_id, created_at);

-- Per-item outcome for batch jobs.
CREATE TABLE IF NOT EXISTS ingest_job_items (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    state TEXT NOT NULL,                -- ok | failed | skipped
    document_id TEXT,
    error_detail TEXT,
    FOREIGN KEY(job_id) REFERENCES ingest_jobs(id)
);
CREATE INDEX IF NOT EXISTS idx_ingest_items_job ON ingest_job_items(job_id);
```

Existing migrations are immutable. `sources` and `document_versions` are
unchanged: `source_type` already exists on `sources`, and versioning already
keys on `(document_id, content_hash)`.

`DocumentRepo::list` gains an optional `source_type` filter and returns the
new `source_type` / `title` columns. `save_versioned` writes them from the
`Document` value instead of hard-coding `"web"`.

### 8. Failure Handling

| Failure | Behavior |
|---------|----------|
| Empty / whitespace-only content | Reject `EMPTY_CONTENT`; nothing written. |
| Content over the size bound | Reject `CONTENT_TOO_LARGE`; nothing written. |
| Invalid UTF-8 | Reject `INVALID_ENCODING` with the byte offset; caller converts and retries. |
| Unreadable file (CLI) | Per-item failure; batch continues; job marked `partial`. |
| Duplicate `source_key`, same content | `VersionDiff::Unchanged`; no new version, no re-index (RFC-006 6.4). |
| Duplicate `source_key`, changed content | New version; old version retained; new outbox events. |
| Outbox index write fails | Existing retry / DLQ path (RFC-006 6.7, RFC-014 14.4). |
| Batch item fails | Item recorded failed; other items proceed; job ends `partial`. |
| Workspace denied | `WORKSPACE_DENIED`; no read or write. |

### 9. Security Considerations

1. **No network.** Ingestion performs no fetch and no SSRF surface.
2. **Untrusted content is data.** HTML ingested from a file is passed through
   the same `extract_html` sanitizer as fetched HTML (scripts/styles stripped,
   RFC-014 14.2). No ingested content is ever executed or interpreted as
   instructions.
3. **Size bounds.** Default 20 MB per document and 200 MB per batch,
   configurable under `ingest.max_bytes_per_document` /
   `ingest.max_bytes_per_batch`. Bounds are checked **before** the object-store
   write.
4. **Filename trust.** HTTP multipart filenames are labels only; they never
   touch the server filesystem and never become object-store keys. Object
   refs are content-addressed (`sha256:<hex>`).
5. **Content-type sniffing.** The declared `content_type` is a hint; the
   sniffer uses magic bytes, then extension, then falls back to `text/plain`.
   Unknown binary types are rejected unless an extension registers an
   extractor.
6. **Workspace isolation.** Every query and write is scoped by `workspace_id`
   (ADR-023). A local document in workspace A is invisible to workspace B
   unless it was explicitly indexed into the global `*` namespace.
7. **CLI path safety.** `--file` / `--dir` are explicit user choices on the
   user's own machine; no path traversal beyond the user's own rights is
   possible. Directory walks skip dotfiles and honor an explicit extension
   allowlist.
8. **Secret handling.** Imported content is user-trusted. It is stored like any
   document and is covered by the existing export redaction rules (RFC-024
   24.5) — document this to the user at ingest time.

### 10. Performance Considerations

- **Deterministic chunking** (RFC-006 6.2) means identical chunks keep their
  `chunk_hash` and reuse embeddings.
- **Per-document outbox transaction** matches the fetch path; no new locking.
- **Directory import** walks once, sorts entries, and processes sequentially
  to bound memory. A future streaming mode is out of scope.
- **Index writes are asynchronous** via the existing outbox worker. Ingest
  returns after the SQLite commit; documents become searchable on the next
  outbox drain (the runtime already drains on startup, and the gateway drains
  every 1.5 s).
- **Rebuildability** (ADR-022) is inherited: `rebuild_from` reconstructs the
  index from SQLite + object store, including local documents.

### 11. Acceptance Criteria

| ID | Criterion |
|----|-----------|
| AC1 | `POST /ingest` with text returns 201, and `mode=local_only` search returns the content. |
| AC2 | `searchos ingest --file notes.md` then `searchos search "..." --mode local-only` returns the content. |
| AC3 | Re-ingesting the same `source_key` with identical content returns `changed=false`, creates no new version and no new outbox row. |
| AC4 | Re-ingesting the same `source_key` with new content creates a new version and retains the old one. |
| AC5 | A local document older than `strict_max_age_seconds` is **not** reported stale. |
| AC6 | A local document in workspace A is not retrievable from workspace B. |
| AC7 | A file above the size bound is rejected with no partial document and no object-store write. |
| AC8 | `searchos document list --source-type local` lists imported documents with title and canonical local URI. |
| AC9 | A batch directory import reports per-item outcomes and ends `completed` or `partial`; failed items do not abort the batch. |
| AC10 | `rebuild_from` reconstructs imported documents identically. |
| AC11 | All pre-existing tests stay green; new unit + integration + CLI tests cover AC1–AC10. |

### 12. Dependencies

| Dependency | Use |
|------------|-----|
| `searchos-domain` | `Document.source_type` / `title`, `ContentCategory::Local`, IDs |
| `searchos-document` | chunking, `VersionDiff`, hash, extractor version constants |
| `searchos-storage-sqlite` | `DocumentRepo` (write + list), migration v4, `ingest_jobs` repos |
| `searchos-fetch` | reuse `extract_html` for HTML files |
| `searchos-application` | new `IngestService`, sibling to `FetchService` |
| `searchos-core` | runtime wiring, `process_outbox` drain |
| `searchos-interface-http` | `/ingest*` routes |
| `searchos-interface-cli` | `searchos ingest` |
| `searchos-policy` | `ContentCategory::Local`, freshness exemption |
| `searchos-retrieval` | `SearchIndex` port (unchanged) |

No new third-party crates are required for v1; the standard library handles
directory walking.

### 13. Implementation Tasks

| ID | Task |
|----|------|
| T1 | Domain: add `Document.source_type` / `title`; add `ContentCategory::Local` + freshness exemption. |
| T2 | Migration v4: `documents.source_type` / `title`, `ingest_jobs`, `ingest_job_items`. |
| T3 | SQLite: generalize `save_versioned` source type/domain; add `DocumentRepo::list_by_source_type`; add ingest-job repos. |
| T4 | Application: `IngestService` (validate → extract → hash → build → `save_versioned`), text/file/batch. |
| T5 | Core: wire `IngestService` into `Runtime`, expose accessor. |
| T6 | HTTP: `/ingest`, `/ingest/file`, `/ingest/batch`, `/ingest/jobs/:id`. |
| T7 | CLI: `searchos ingest` + `document list --source-type`. |
| T8 | Docs: RFC-029, ADR-028, RFC index, docs index, CLI/API reference. |
| T9 | Tests: unit (versioning/dedup/freshness), integration (local_only retrieval), CLI smoke, size-bound tests. |

---

## 中文

### 1. 背景

SearchOS 是本地优先的：决策引擎会先评估本地知识，再决定是否花钱调外部
Provider；`mode=local_only` 会禁止一切外部调用。但目前唯一的入库方式是
**URL 抓取**（`POST /extract`、`POST /documents`、CLI `searchos fetch`）。
用户自己的资料——一段粘贴的笔记、一个 Markdown 文件、一份 CSV 导出、整个
文档目录——都无法进入 `local_only` / `local_first` 已经在查询的同一个知识库。

本 RFC 补上这个缺口。本地导入**不能**变成一条平行管线：它必须复用 URL
抓取已经在用的同一套文档生命周期（RFC-006）、同一个事务 outbox
（RFC-006 6.7）、同一个 `SearchIndex` 端口（RFC-002 2.4）和同一个
`LocalKnowledgePort`（RFC-004）。否则本地数据在检索、质量门和索引重建里
就是二等公民。

### 2. 目标

| 编号 | 目标 |
|----|------|
| G1 | 支持导入纯文本、单个文件和目录，进入知识库。 |
| G2 | 复用现有 document/version/chunk/outbox 链路，不重复实现索引逻辑。 |
| G3 | 导入内容在 `local_only`、`local_first`、`hybrid` 下与抓取内容完全一致地可检索。 |
| G4 | 导入内容**不会**仅因为存放时间长而被判定为 stale——数据属于用户。 |
| G5 | 安全导入：显式大小上限、内容类型嗅探、服务端不信任客户端文件名。 |
| G6 | 两种运行模式都可用：CLI（Local Mode，进程内）和 HTTP Gateway。 |
| G7 | 去重与版本语义对齐 URL 抓取：同一逻辑来源 + 同一内容 = 无操作；内容变化 = 新版本。 |

### 3. 非目标

| 编号 | 非目标 |
|----|----------|
| N1 | v1 核心不解析 PDF / DOCX / PPTX 等二进制办公格式。只预留扩展点，官方 extractor 后续发布。 |
| N2 | 文件监听、自动同步、写回源文件。 |
| N3 | OCR、音频、视频抽取。 |
| N4 | 对导入内容做密钥 / PII 扫描。导入内容按用户可信数据处理。 |
| N5 | 分布式 / 多节点导入。 |
| N6 | 改变 `local_only` / `local_first` 的决策语义。数据入库后它们已经能正确工作。 |

### 4. 架构

```text
   CLI: searchos ingest --text | --file | --dir
   HTTP: POST /ingest | /ingest/file | /ingest/batch
                    │
                    ▼
        ┌───────────────────────────┐
        │   IngestService (应用层)   │
        │  校验 → 抽取 → 内容哈希 →  │
        │  构建 Document/Version/Chunk│
        └─────────────┬─────────────┘
                      │  同一个 DocumentPort
                      ▼
        ┌───────────────────────────┐
        │ SQLite + 对象存储          │
        │ documents / versions /    │
        │ chunks / index_outbox     │
        └─────────────┬─────────────┘
                      │  已有 outbox worker
                      ▼
        ┌───────────────────────────┐
        │ SearchIndex 端口          │
        │ Tantivy | ES | Meili | PG │
        │ | MySQL (ADR-027)         │
        └─────────────┬─────────────┘
                      │
                      ▼
        ┌───────────────────────────┐
        │ LocalKnowledgePort        │
        │ → 决策引擎                │
        │ (local_candidates)        │
        └───────────────────────────┘
```

`IngestService` 是 **`FetchService` 的同级**，不是替代者。两者都调用
`DocumentPort::save_versioned`，因此版本化、切块、outbox 事件、索引重建和
工作区隔离全部免费继承。

### 5. 领域模型

#### 5.1 来源身份

本地内容没有 URL。SearchOS 分配一个**规范化本地 URI**：

```text
searchos-local://<workspace_id>/<source_key>
```

- `source_key` 是来源的逻辑身份。
- 调用方提供 `source_key` 时，会先规范化（trim、转小写、路径分隔符合并为
  `/`、去掉前导 `/`），再原样使用。
- 调用方未提供时，SearchOS 派生
  `source_key = sha256(content)[:16]`，即来源**按内容寻址**。相同内容去重为
  一个文档；内容变化则成为新文档，除非调用方固定了一个稳定 key。

这与 URL 抓取一致：canonical URL 是身份，content hash 是版本判据。

#### 5.2 Document 字段

`Document` 新增两个字段（向后兼容，serde 默认值）：

```rust
pub struct Document {
    // ... 现有字段 ...
    /// "web" | "local_text" | "local_file" | "local_directory"
    pub source_type: String,
    /// 人类可读标题（文件名、文档标题或调用方指定）
    pub title: Option<String>,
}
```

`source_type` 写入已有的 `sources.source_type` 列，不再硬编码 `"web"`。本地
来源的 `domain` 固定为 `"local"`；web 来源仍是 `domain_of(canonical_url)`。

#### 5.3 导入请求

```rust
pub struct IngestRequest {
    pub source_key: Option<String>,
    pub title: Option<String>,
    pub content_type: Option<String>,   // 提示；缺失时嗅探
    pub language: Option<String>,
    pub workspace_id: WorkspaceId,
    pub metadata: serde_json::Value,    // 调用方标签，核心不查询
}

pub enum IngestContent {
    Text(String),
    File { path: PathBuf, file_name: String },
    Batch(Vec<(IngestRequest, IngestContent)>),
}
```

#### 5.4 时效类别

新增 `ContentCategory::Local`，其 `default_ttl_seconds() = None`
（永不因年龄 stale）。`FreshnessPolicy::is_fresh` 对本地候选默认返回
`true`，除非调用方在 `ttl_overrides["local"]` 显式覆盖。这样才能保证用户
自己的笔记不会被在一年后被判定为过期并触发重新抓取。

### 6. 接口契约

#### 6.1 HTTP（Gateway Mode）

| 方法 | 路径 | 请求体 | 说明 |
|--------|------|------|-------|
| `POST` | `/ingest` | JSON `IngestRequest` + `content` 字符串 | 文本导入 |
| `POST` | `/ingest/file` | `multipart/form-data`：`file` + 元数据字段 | 文件上传 |
| `POST` | `/ingest/batch` | JSON 数组 `{request, content}` | 目录 / 批量 |
| `GET` | `/ingest/jobs/:id` | — | 批次任务状态 |

响应信封（与现有网关风格一致）：

```json
{
  "document_id": "doc_...",
  "version_id": "docv_...",
  "canonical_url": "searchos-local://default/notes/alpha",
  "source_type": "local_file",
  "content_hash": "sha256:...",
  "changed": true,
  "title": "alpha.md",
  "chunk_count": 12,
  "status": "indexed"
}
```

错误码：`EMPTY_CONTENT`、`CONTENT_TOO_LARGE`、`INVALID_ENCODING`、
`UNSUPPORTED_CONTENT_TYPE`、`SOURCE_KEY_CONFLICT`、`WORKSPACE_DENIED`。
所有 handler 都与 `/extract` 一样做认证和 `authorize_workspace`。

#### 6.2 CLI（Local Mode，进程内）

```bash
searchos ingest --text "..." [--title "..."] [--workspace ws] [--source-key k]
searchos ingest --file ./notes.md [--source-key notes/alpha] [--workspace ws]
searchos ingest --dir ./docs --recursive [--ext md,txt] [--workspace ws]
searchos ingest --batch ./jobs.json
```

`searchos document list` 增加 `--source-type web|local` 和 `--workspace`。
`searchos document inspect` 无需改动即可使用。

#### 6.3 MCP

MCP 暴露 RFC-021 的 7 个工具与 `search_trace` 兼容别名，现在也暴露
`ingest_text` / `ingest_file`（RFC-021 后续修订）。端口已可从 MCP handler
以进程内方式调用；接受文本与内存文件字节，绝不从 MCP 接受服务端文件系统路径。

### 7. 数据模型

Migration **v4**（`local_ingestion`）追加：

```sql
-- 文档来源：本地 vs web，以及人类可读标题
ALTER TABLE documents ADD COLUMN source_type TEXT NOT NULL DEFAULT 'web';
ALTER TABLE documents ADD COLUMN title TEXT;

-- 批量 / 目录导入任务（可观测 + 可恢复）
CREATE TABLE IF NOT EXISTS ingest_jobs (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    source_type TEXT NOT NULL,          -- local_text | local_file | local_directory
    state TEXT NOT NULL,                -- pending | running | completed | partial | failed
    total_items INTEGER NOT NULL DEFAULT 0,
    ok_items INTEGER NOT NULL DEFAULT 0,
    failed_items INTEGER NOT NULL DEFAULT 0,
    error_detail TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_ingest_jobs_ws ON ingest_jobs(workspace_id, created_at);

-- 批次任务的逐项结果
CREATE TABLE IF NOT EXISTS ingest_job_items (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    state TEXT NOT NULL,                -- ok | failed | skipped
    document_id TEXT,
    error_detail TEXT,
    FOREIGN KEY(job_id) REFERENCES ingest_jobs(id)
);
CREATE INDEX IF NOT EXISTS idx_ingest_items_job ON ingest_job_items(job_id);
```

已发布 migration 不可修改。`sources` 和 `document_versions` 不变：
`sources.source_type` 已存在，版本化已以 `(document_id, content_hash)` 为键。

`DocumentRepo::list` 增加可选 `source_type` 过滤，并返回新增的
`source_type` / `title` 列。`save_versioned` 从 `Document` 值写入这两个字段，
不再硬编码 `"web"`。

### 8. 故障处理

| 故障 | 行为 |
|---------|----------|
| 空内容 / 纯空白 | 拒绝 `EMPTY_CONTENT`；不写入任何东西。 |
| 超过大小上限 | 拒绝 `CONTENT_TOO_LARGE`；不写入任何东西。 |
|非法 UTF-8 | 拒绝 `INVALID_ENCODING` 并给出字节偏移；调用方转换后重试。 |
| 文件不可读（CLI） | 单项目失败；批次继续；任务标记 `partial`。 |
| 同一 `source_key`、内容相同 | `VersionDiff::Unchanged`；不新建版本、不重新索引（RFC-006 6.4）。 |
| 同一 `source_key`、内容变化 | 新建版本；保留旧版本；产生新 outbox 事件。 |
| outbox 索引写入失败 | 走现有重试 / DLQ 路径（RFC-006 6.7、RFC-014 14.4）。 |
| 批次中某项失败 | 记录该项失败；其余继续；任务最终为 `partial`。 |
| 工作区拒绝 | `WORKSPACE_DENIED`；不读不写。 |

### 9. 安全考量

1. **不联网。** 导入不发起任何 fetch，没有 SSRF 攻击面。
2. **不可信内容即数据。** 从文件导入的 HTML 走与抓取 HTML 相同的
   `extract_html` 清洗器（剥离 script/style，RFC-014 14.2）。导入内容永远
   不被执行，也不被当作指令解释。
3. **大小上限。** 默认单文档 20 MB、单批次 200 MB，可在
   `ingest.max_bytes_per_document` / `ingest.max_bytes_per_batch` 配置。上限
   在写对象存储**之前**校验。
4. **文件名信任。** HTTP multipart 的文件名只是标签，绝不落在服务端文件系统
   上，也绝不成为对象存储 key。对象引用按内容寻址（`sha256:<hex>`）。
5. **内容类型嗅探。** 声明的 `content_type` 只是提示；嗅探器先看 magic
   bytes，再看扩展名，最后回落到 `text/plain`。未注册 extractor 的未知二进制
   类型一律拒绝。
6. **工作区隔离。** 每次查询和写入都按 `workspace_id` 限定（ADR-023）。工作区
   A 的本地文档对工作区 B 不可见，除非显式索引进全局 `*` 命名空间。
7. **CLI 路径安全。** `--file` / `--dir` 是用户在自己机器上的显式选择，不
   可能超出用户自身权限。目录遍历跳过点开头的文件，并遵守显式扩展名白名单。
8. **密钥处理。** 导入内容按用户可信处理。它与普通文档一样存储，并受现有
   导出脱敏规则约束（RFC-024 24.5）——导入时需要向用户明确说明这一点。

### 10. 性能考量

- **确定性切块**（RFC-006 6.2）保证相同 chunk 保持相同 `chunk_hash`，复用
  embedding。
- **每文档一个 outbox 事务**，与抓取路径一致，不引入新的锁。
- **目录导入** 只遍历一次，排序后串行处理以限制内存。流式模式不在范围内。
- **索引写入是异步的**，走已有 outbox worker。导入在 SQLite 提交后返回；文档
  在下一轮 outbox drain 后可检索（runtime 启动时已 drain 一次，gateway 每
  1.5 秒 drain 一次）。
- **可重建**（ADR-022）免费继承：`rebuild_from` 从 SQLite + 对象存储重建索引，
  本地文档同样适用。

### 11. 验收标准

| 编号 | 标准 |
|----|-----------|
| AC1 | `POST /ingest` 传入文本返回 201，且 `mode=local_only` 搜索能返回该内容。 |
| AC2 | `searchos ingest --file notes.md` 后 `searchos search "..." --mode local-only` 能返回该内容。 |
| AC3 | 同一 `source_key` 重复导入相同内容返回 `changed=false`，不新建版本、不产生新 outbox 行。 |
| AC4 | 同一 `source_key` 导入新内容会新建版本并保留旧版本。 |
| AC5 | 存放时间超过 `strict_max_age_seconds` 的本地文档**不会**被判定为 stale。 |
| AC6 | 工作区 A 的本地文档无法被工作区 B 检索。 |
| AC7 | 超过大小上限的文件被拒绝，不产生部分文档，也不写对象存储。 |
| AC8 | `searchos document list --source-type local` 列出导入文档及其标题和本地 URI。 |
| AC9 | 目录批次导入报告逐项结果，最终为 `completed` 或 `partial`；单项失败不中断整批。 |
| AC10 | `rebuild_from` 能一致地重建导入的文档。 |
| AC11 | 既有测试全部保持绿色；新增单元 + 集成 + CLI 测试覆盖 AC1–AC10。 |

### 12. 依赖

| 依赖 | 用途 |
|------------|-----|
| `searchos-domain` | `Document.source_type` / `title`、`ContentCategory::Local`、ID |
| `searchos-document` | 切块、`VersionDiff`、hash、extractor 版本常量 |
| `searchos-storage-sqlite` | `DocumentRepo`（写入 + 列表）、migration v4、`ingest_jobs` repos |
| `searchos-fetch` | 复用 `extract_html` 处理 HTML 文件 |
| `searchos-application` | 新增 `IngestService`，与 `FetchService` 同级 |
| `searchos-core` | runtime 组装、`process_outbox` drain |
| `searchos-interface-http` | `/ingest*` 路由 |
| `searchos-interface-cli` | `searchos ingest` |
| `searchos-policy` | `ContentCategory::Local`、freshness 豁免 |
| `searchos-retrieval` | `SearchIndex` 端口（不变） |

v1 不需要新的第三方 crate；目录遍历用标准库即可。

### 13. 实施任务

| 编号 | 任务 |
|----|------|
| T1 | 领域层：`Document.source_type` / `title`；`ContentCategory::Local` + freshness 豁免。 |
| T2 | Migration v4：`documents.source_type` / `title`、`ingest_jobs`、`ingest_job_items`。 |
| T3 | SQLite：泛化 `save_versioned` 的 source type/domain；新增 `DocumentRepo::list_by_source_type`；新增 ingest-job repos。 |
| T4 | 应用层：`IngestService`（校验 → 抽取 → hash → 构建 → `save_versioned`），支持文本/文件/批次。 |
| T5 | Core：把 `IngestService` 接入 `Runtime`，暴露 accessor。 |
| T6 | HTTP：`/ingest`、`/ingest/file`、`/ingest/batch`、`/ingest/jobs/:id`。 |
| T7 | CLI：`searchos ingest` + `document list --source-type`。 |
| T8 | 文档：RFC-029、ADR-028、RFC 索引、docs 索引、CLI/API 参考。 |
| T9 | 测试：单元（版本化/去重/freshness）、集成（local_only 检索）、CLI 冒烟、大小上限测试。 |
