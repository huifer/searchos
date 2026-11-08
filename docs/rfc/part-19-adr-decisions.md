# Part 19. ADR Decisions

以下架构决策作为第一阶段默认基线。

| ADR     | 决策                                     |
| ------- | -------------------------------------- |
| ADR-001 | Rust Workspace                         |
| ADR-002 | Core / Application / Interface 解耦      |
| ADR-003 | Local + Gateway 双运行模式                  |
| ADR-004 | Local In-Process Execution             |
| ADR-005 | Axum HTTP Gateway                      |
| ADR-006 | Tavily-compatible API                  |
| ADR-007 | SQLite Metadata Store                  |
| ADR-008 | Tantivy Full-Text Index                |
| ADR-009 | LanceDB Vector Index                   |
| ADR-010 | Filesystem Content Store               |
| ADR-011 | Transactional Outbox                   |
| ADR-012 | Cache / Document / Memory 分离           |
| ADR-013 | Search IR                              |
| ADR-014 | Quality-Gated Search Decision          |
| ADR-015 | Cost Ledger Append-Only                |
| ADR-016 | Provider Registry                      |
| ADR-017 | Site-Specific Crawler Directory        |
| ADR-018 | Declarative + Native + WASM Extensions |
| ADR-019 | Crawler Failure Isolation              |
| ADR-020 | Search Profile Versioning              |
| ADR-021 | Evidence Provenance                    |
| ADR-022 | Local Index Rebuildability             |
| ADR-023 | Workspace Isolation                    |
| ADR-024 | Cost-Aware Execution Planner           |
| ADR-025 | Evaluation-Driven Quality              |
| ADR-026 | Open Core Distribution                 |

## 19.1 ADR-026: Open Core

开源核心建议包括：

- Local Runtime。
- Gateway Runtime。
- CLI。
- MCP。
- Core Search。
- Exact Cache。
- BM25。
- Vector Retrieval。
- Search Decision。
- Cost Ledger。
- Provider Interface。
- Generic Crawler。
- Extension Interface。

商业化功能可以包括：

- Hosted Gateway。
- Multi-Tenant。
- SSO。
- RBAC。
- Distributed Search。
- Centralized Cost Governance。
- Managed Crawler。
- Team Analytics。
- SLA。

不要故意削弱本地搜索质量来制造付费门槛。
