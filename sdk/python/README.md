# searchos-sdk (Python)

Typed Python client for the SearchOS Gateway API.

Generated from [`docs/openapi.yaml`](../../docs/openapi.yaml). That document is
the frozen external contract and is verified against the real Axum route table
by the Rust test `openapi_spec_matches_the_route_table`, so a method here
cannot drift from the gateway without CI failing.

## Install

```bash
pip install searchos-sdk
```

## Use

```python
from searchos import SearchOSClient, SearchOSError

searchos = SearchOSClient(
    base_url="http://127.0.0.1:8787",
    base_path="/v1",                      # optional; the same paths are served unversioned
    token=None,                           # set when server.auth_token is configured
)

response = searchos.search("hybrid retrieval RRF fusion", mode="local-only", max_results=5)
# Status is explicit: partial / insufficient_evidence are never "completed".
print(response["status"], len(response["results"]), response.get("trace_id"))

# Import local text into the same knowledge store as URL fetches.
doc = searchos.ingest_text(
    "SearchOS serves evidence, not generated answers.",
    source_key="notes/searchos",
    title="SearchOS note",
)
print(doc["document_id"], doc["changed"])

# Inspect, re-index, then delete — the document lifecycle.
detail = searchos.get_document(doc["document_id"])
print(detail["summary"]["status"], len(detail["versions"]))
searchos.reindex_document(doc["document_id"])
searchos.delete_document(doc["document_id"])

# Analytics Read Model: filters are pushed into SQL and the meta declares
# which were applied and which the dataset cannot apply.
overview = searchos.dashboard_overview(interval="week")
print(overview["meta"]["query_mode"], overview["meta"]["applied_filters"])

# Errors carry the gateway status and error code.
try:
    searchos.get_document("doc_missing")
except SearchOSError as error:
    print(error.status, error.code)
```

## Coverage

Every operation in `docs/openapi.yaml` has a typed method:

| Group | Methods |
|---|---|
| Search | `search` |
| Knowledge | `extract`, `ingest_text`, `ingest_file`, `ingest_batch`, `get_ingest_job`, `list_documents`, `get_document`, `get_document_versions`, `reindex_document`, `delete_document`, `search_documents` |
| Memory | `search_memory` |
| Cost | `cost_summary`, `cost_providers`, `cost_savings`, `cost_reservations`, `reconcile_cost` |
| Observability | `stats_search`, `stats_savings`, `stats_retrieval`, `get_decision`, `list_traces`, `system_status`, `list_profiles`, `get_profile`, `health`, `metrics` |
| Analytics | `dashboard_overview`, `dashboard_dataset`, `dashboard_export` |

## Notes

- The client uses only the standard library (`urllib.request`); there are no
  runtime dependencies.
- `ingest_file` streams the file as multipart/form-data with the filename as a
  label only — the server never writes to its own filesystem.
- SSE (`/api/v1/dashboard/events`) is intentionally not wrapped: it is a
  refetch hint, and the Read Model remains the historical truth.
