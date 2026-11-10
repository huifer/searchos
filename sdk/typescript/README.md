# @searchos/typescript-sdk

Typed TypeScript client for the SearchOS Gateway API.

The client is generated from [`docs/openapi.yaml`](../../docs/openapi.yaml).
That document is the frozen external contract and is verified against the real
Axum route table by the Rust test
`openapi_spec_matches_the_route_table`, so a method here cannot drift from the
gateway without CI failing.

## Install

```bash
npm install @searchos/typescript-sdk
```

## Use

```ts
import { SearchOSClient, SearchOSError } from "@searchos/typescript-sdk";

const searchos = new SearchOSClient({
  baseUrl: "http://127.0.0.1:8787",
  basePath: "/v1",            // optional; the same paths are served unversioned
  token: process.env.SEARCHOS_AUTH_TOKEN,
});

const response = await searchos.search({
  query: "hybrid retrieval RRF fusion",
  mode: "local-only",
  max_results: 5,
});

// Status is explicit: partial / insufficient_evidence are never "completed".
console.log(response.status, response.results.length, response.trace_id);

// Import local text into the same knowledge store as URL fetches.
const doc = await searchos.ingestText({
  content: "SearchOS serves evidence, not generated answers.",
  source_key: "notes/searchos",
  title: "SearchOS note",
});
console.log(doc.document_id, doc.changed);

// Inspect, re-index, then delete — the document lifecycle.
const detail = await searchos.getDocument(doc.document_id);
console.log(detail.summary?.status, detail.versions?.length);
await searchos.reindexDocument(doc.document_id);
await searchos.deleteDocument(doc.document_id);

// Analytics Read Model: filters are pushed into SQL, and the response declares
// which filters were applied and which the dataset cannot apply.
const overview = await searchos.dashboardOverview({ interval: "week" });
console.log(overview.meta?.query_mode, overview.meta?.applied_filters, overview.meta?.ignored_filters);

// Errors carry the gateway status and error code.
try {
  await searchos.getDocument("doc_missing");
} catch (error) {
  if (error instanceof SearchOSError) console.log(error.status, error.code);
}
```

## Coverage

Every operation in `docs/openapi.yaml` has a typed method:

| Group | Methods |
|---|---|
| Search | `search` |
| Knowledge | `extract`, `extractTavily`, `ingestText`, `ingestFile`, `ingestBatch`, `getIngestJob`, `listDocuments`, `getDocument`, `getDocumentVersions`, `reindexDocument`, `deleteDocument`, `searchDocuments` |
| Memory | `searchMemory` |
| Cost | `costSummary`, `costProviders`, `costSavings`, `costReservations`, `reconcileCost` |
| Observability | `statsSearch`, `statsSavings`, `statsRetrieval`, `getDecision`, `listTraces`, `systemStatus`, `listProfiles`, `getProfile`, `health`, `metrics` |
| Analytics | `dashboardOverview`, `dashboardDataset`, `dashboardExport`, `dashboardEvents` |

## Notes

- `dashboardEvents` returns a browser `EventSource`. The SSE stream is a
  refetch hint only; the Read Model remains the historical truth.
- `dashboardExport("csv")` returns raw text, `"json"` returns the envelope.
- `ingestFile` accepts a `Blob`; the runtime sets the multipart boundary.

## Build

```bash
npm install
npm run build
```
