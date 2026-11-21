"""SearchOS Gateway API — typed Python client.

Generated from ``docs/openapi.yaml``. The contract is verified against the
real Axum route table by the Rust test ``openapi_spec_matches_the_route_table``
in ``crates/searchos-interface-http``, so a method here cannot drift from the
gateway without CI failing.

Same paths are served unversioned and under ``/v1``; pass
``base_path="/v1"`` to use the versioned mount.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Literal

import urllib.request
import urllib.error

SearchMode = Literal["local-only", "local-first", "hybrid", "external-only", "external-first"]
SearchStatus = Literal[
    "completed",
    "completed_with_warnings",
    "partial",
    "insufficient_evidence",
    "budget_exhausted",
    "failed",
    "cancelled",
]
Interval = Literal["minute", "hour", "day", "week", "month"]


class SearchOSError(RuntimeError):
    """Gateway rejection, carrying the HTTP status and error code."""

    def __init__(self, message: str, status: int, code: str | None = None) -> None:
        super().__init__(message)
        self.status = status
        self.code = code


@dataclass
class SearchOSClient:
    base_url: str = "http://127.0.0.1:8787"
    base_path: Literal["/v1", ""] = ""
    token: str | None = None
    timeout: float = 30.0

    # -- Search ---------------------------------------------------------------

    def search(
        self,
        query: str,
        *,
        mode: SearchMode = "local-first",
        profile: str | None = None,
        workspace_id: str | None = None,
        request_id: str | None = None,
        max_results: int | None = None,
        max_external_queries: int | None = None,
        max_total_cost: float | None = None,
    ) -> dict[str, Any]:
        body = {"query": query, "mode": mode}
        for key, value in (
            ("profile", profile),
            ("workspace_id", workspace_id),
            ("request_id", request_id),
            ("max_results", max_results),
            ("max_external_queries", max_external_queries),
            ("max_total_cost", max_total_cost),
        ):
            if value is not None:
                body[key] = value
        return self._post("/search", body)

    def search_stream(
        self,
        request: dict[str, Any],
        on_event: Any = None,
    ) -> list[dict[str, Any]]:
        """RFC-056: consume the typed SSE stream; the final event carries the full response."""
        text = self._request(
            "POST",
            "/search/stream",
            body=json.dumps(request).encode(),
            content_type="application/json",
            parse="text",
        )
        events = _parse_sse(text)
        if on_event is not None:
            for event in events:
                on_event(event)
        return events

    def fetch_batch(
        self,
        urls: list[str],
        *,
        workspace_id: str | None = None,
        persist: bool = True,
        max_urls: int = 20,
        max_per_host: int = 64,
        default_interval_ms: int = 250,
        max_total_ms: int = 60_000,
    ) -> dict[str, Any]:
        """RFC-051: bounded batch fetch through the cross-host crawl frontier."""
        body: dict[str, Any] = {
            "urls": urls,
            "persist": persist,
            "max_urls": max_urls,
            "max_per_host": max_per_host,
            "default_interval_ms": default_interval_ms,
            "max_total_ms": max_total_ms,
        }
        if workspace_id is not None:
            body["workspace_id"] = workspace_id
        return self._post("/fetch/batch", body)

    # -- Knowledge ------------------------------------------------------------

    def extract(self, url: str, *, workspace_id: str | None = None, persist: bool = True) -> dict[str, Any]:
        return self._post("/extract", {"url": url, "workspace_id": workspace_id, "persist": persist})

    def ingest_text(
        self,
        content: str,
        *,
        source_key: str | None = None,
        title: str | None = None,
        workspace_id: str | None = None,
        content_type: str | None = None,
        language: str | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"content": content}
        for key, value in (
            ("source_key", source_key),
            ("title", title),
            ("workspace_id", workspace_id),
            ("content_type", content_type),
            ("language", language),
        ):
            if value is not None:
                body[key] = value
        return self._post("/ingest", body)

    def ingest_file(
        self,
        path: str,
        *,
        source_key: str | None = None,
        title: str | None = None,
        workspace_id: str | None = None,
    ) -> dict[str, Any]:
        """Uploads a local text file as multipart/form-data."""
        boundary = "----searchos-python-boundary"
        import mimetypes
        import os

        filename = os.path.basename(path)
        content_type = mimetypes.guess_type(filename)[0] or "application/octet-stream"
        with open(path, "rb") as handle:
            payload = handle.read()

        parts: list[bytes] = []
        fields = {"source_key": source_key, "title": title, "workspace_id": workspace_id}
        for name, value in fields.items():
            if value is None:
                continue
            parts.append(f"--{boundary}\r\n".encode())
            parts.append(f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode())
            parts.append(str(value).encode())
            parts.append(b"\r\n")
        parts.append(f"--{boundary}\r\n".encode())
        parts.append(
            f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode()
        )
        parts.append(f"Content-Type: {content_type}\r\n\r\n".encode())
        parts.append(payload)
        parts.append(f"\r\n--{boundary}--\r\n".encode())

        return self._request(
            "POST",
            "/ingest/file",
            body=b"".join(parts),
            content_type=f"multipart/form-data; boundary={boundary}",
        )

    def ingest_batch(self, items: list[dict[str, Any]]) -> dict[str, Any]:
        return self._post("/ingest/batch", {"items": items})

    def get_ingest_job(self, job_id: str) -> dict[str, Any]:
        return self._get(f"/ingest/jobs/{job_id}")

    def list_documents(
        self,
        *,
        workspace_id: str | None = None,
        source_type: str | None = None,
        status: str | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        query: dict[str, Any] = {"limit": limit}
        for key, value in (
            ("workspace_id", workspace_id),
            ("source_type", source_type),
            ("status", status),
        ):
            if value is not None:
                query[key] = value
        return self._get("/documents", query).get("documents", [])

    def create_document(
        self, url: str, *, workspace_id: str | None = None
    ) -> dict[str, Any]:
        """Fetch and persist a URL as a document."""
        body: dict[str, Any] = {"url": url}
        if workspace_id is not None:
            body["workspace_id"] = workspace_id
        return self._post("/documents", body)

    def get_document(self, document_id: str) -> dict[str, Any]:
        return self._get(f"/documents/{document_id}")

    def get_document_versions(self, document_id: str) -> list[dict[str, Any]]:
        return self._get(f"/documents/{document_id}/versions").get("versions", [])

    def reindex_document(self, document_id: str) -> dict[str, Any]:
        return self._post(f"/documents/{document_id}/reindex", {})

    def delete_document(self, document_id: str) -> dict[str, Any]:
        return self._request("DELETE", f"/documents/{document_id}")

    def search_documents(
        self, query: str, *, workspace_id: str | None = None, limit: int = 20
    ) -> list[dict[str, Any]]:
        body: dict[str, Any] = {"query": query, "limit": limit}
        if workspace_id is not None:
            body["workspace_id"] = workspace_id
        return self._post("/documents/search", body).get("documents", [])

    # -- Memory ---------------------------------------------------------------

    def search_memory(self, query: str, *, workspace_id: str | None = None, top_k: int = 10) -> list[dict[str, Any]]:
        body: dict[str, Any] = {"query": query, "top_k": top_k}
        if workspace_id is not None:
            body["workspace_id"] = workspace_id
        return self._post("/memory/search", body).get("results", [])

    # -- Cost -----------------------------------------------------------------

    def cost_summary(self) -> dict[str, Any]:
        return self._get("/cost/summary")

    def cost_providers(self) -> dict[str, Any]:
        return self._get("/cost/providers")

    def cost_savings(self) -> dict[str, Any]:
        return self._get("/cost/savings")

    def cost_reservations(self) -> list[dict[str, Any]]:
        return self._get("/cost/reservations").get("reservations", [])

    def cost_invoices(
        self,
        *,
        workspace_id: str | None = None,
        start: str | None = None,
        end: str | None = None,
    ) -> dict[str, Any]:
        """RFC-042 invoice view over the append-only ledger, grouped by currency."""
        query: dict[str, Any] = {}
        for key, value in (("workspace_id", workspace_id), ("from", start), ("to", end)):
            if value is not None:
                query[key] = value
        return self._get("/cost/invoices", query)

    def reconcile_cost(
        self, cost_event_id: str, actual_cost_micros: int, source: str, currency: str = "USD"
    ) -> dict[str, Any]:
        return self._post(
            "/cost/reconcile",
            {
                "cost_event_id": cost_event_id,
                "actual_cost_micros": actual_cost_micros,
                "source": source,
                "currency": currency,
            },
        )

    # -- Observability --------------------------------------------------------

    def stats_search(self) -> dict[str, Any]:
        return self._get("/stats/search")

    def stats_savings(self) -> dict[str, Any]:
        return self._get("/stats/savings")

    def stats_retrieval(self) -> dict[str, Any]:
        return self._get("/stats/retrieval")

    def get_decision(self, trace_id: str) -> dict[str, Any]:
        return self._get(f"/decisions/{trace_id}")

    def list_traces(self, limit: int = 20) -> list[dict[str, Any]]:
        return self._get("/traces", {"limit": limit}).get("traces", [])

    # -- Durable background jobs (RFC-054) ------------------------------------

    def list_jobs(
        self,
        *,
        workspace_id: str | None = None,
        state: str | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        query: dict[str, Any] = {"limit": limit}
        for key, value in (("workspace_id", workspace_id), ("state", state)):
            if value is not None:
                query[key] = value
        return self._get("/jobs", query).get("jobs", [])

    def enqueue_job(
        self,
        kind: str,
        *,
        payload: dict[str, Any] | None = None,
        workspace_id: str | None = None,
        max_attempts: int = 3,
        run_after: str | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {
            "kind": kind,
            "payload": payload or {},
            "max_attempts": max_attempts,
        }
        if workspace_id is not None:
            body["workspace_id"] = workspace_id
        if run_after is not None:
            body["run_after"] = run_after
        return self._post("/jobs", body)

    def get_job(self, job_id: str) -> dict[str, Any]:
        return self._get(f"/jobs/{job_id}")

    def system_status(self) -> dict[str, Any]:
        return self._get("/system/status")

    def list_profiles(self) -> list[dict[str, Any]]:
        return self._get("/profiles").get("profiles", [])

    def get_profile(self, profile_id: str) -> dict[str, Any]:
        return self._get(f"/profiles/{profile_id}")

    def health(self) -> dict[str, Any]:
        return self._get("/health")

    def metrics(self) -> str:
        """Prometheus text exposition format 0.0.4."""
        return self._request("GET", "/metrics", parse="text")

    def search_follow_up(self, request: dict[str, Any]) -> dict[str, Any]:
        """Continue an agent session; reuses earlier evidence when still valid."""
        return self._post("/search/follow-up", request)

    # -- Evidence acquisition / reuse (RFC-031) --------------------------------

    def evidence_acquisitions(self) -> dict[str, Any]:
        return self._get("/evidence/acquisitions")

    def evidence_acquisition(self, acquisition_id: str) -> dict[str, Any]:
        return self._get(f"/evidence/acquisitions/{acquisition_id}")

    def evidence_reuse(self) -> dict[str, Any]:
        return self._get("/evidence/reuse")

    def trace(self, trace_id: str) -> dict[str, Any]:
        """Raw recorded decision trace at the canonical path."""
        return self._get(f"/trace/{trace_id}")

    # -- Structured extraction (RFC-049) ---------------------------------------

    def extract_structures(
        self, *, text: str | None = None, url: str | None = None
    ) -> dict[str, Any]:
        """Deterministic structures: code blocks, tables, lists, headings, links."""
        body: dict[str, Any] = {}
        if text is not None:
            body["text"] = text
        if url is not None:
            body["url"] = url
        return self._post("/structures", body)

    # -- Online feedback (RFC-033) --------------------------------------------

    def submit_feedback(
        self,
        *,
        query: str,
        url: str,
        verdict: Literal["canonical", "helpful", "related", "irrelevant", "wrong"],
        workspace_id: str = "default",
        task_id: str | None = None,
        note: str | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {
            "workspace_id": workspace_id,
            "query": query,
            "url": url,
            "verdict": verdict,
        }
        if task_id:
            body["task_id"] = task_id
        if note:
            body["note"] = note
        return self._post("/feedback", body)

    def list_feedback(self, workspace_id: str = "default", limit: int = 50) -> list[dict[str, Any]]:
        return self._get("/feedback", {"workspace_id": workspace_id, "limit": limit}).get("feedback", [])

    def feedback_qrels(self, workspace_id: str = "default", min_agreement: int = 2) -> dict[str, Any]:
        return self._get(
            "/feedback/qrels",
            {"workspace_id": workspace_id, "min_agreement": min_agreement},
        )

    def feedback_dataset(self, workspace_id: str = "default", min_agreement: int = 2) -> str:
        return self._request(
            "GET",
            f"/feedback/dataset?workspace_id={workspace_id}&min_agreement={min_agreement}",
            parse="text",
        )

    # -- Publisher trust (RFC-032) ---------------------------------------------

    def publisher_trust(self, workspace_id: str = "default") -> list[dict[str, Any]]:
        """Per-publisher trust from judged results (unknown below the sample floor)."""
        return self._get("/publishers", {"workspace_id": workspace_id}).get("publishers", [])

    # -- Search Watch (RFC-035) -----------------------------------------------

    def watch_run(self) -> dict[str, Any]:
        """Run every applicable check and append one snapshot."""
        return self._post("/watch/run", {})

    def watch_snapshots(self, limit: int = 20) -> list[dict[str, Any]]:
        return self._get("/watch/snapshots", {"limit": limit}).get("snapshots", [])

    # -- Planning (RFC-031/004) ------------------------------------------------

    def search_plan(self, request: dict[str, Any]) -> dict[str, Any]:
        """Plan-only: what WOULD be searched, without buying anything."""
        return self._post("/search/plan", request)

    # -- Research workspace (RFC-071) ------------------------------------------

    def research_run(self, body: dict[str, Any]) -> dict[str, Any]:
        """Bounded deep search; each round is a full search recorded as a turn."""
        return self._post("/research/run", body)

    def get_research_workspace(self, session_id: str) -> dict[str, Any]:
        return self._get(f"/research/{session_id}")

    # -- Entities (RFC-058) ----------------------------------------------------

    def list_entities(self) -> list[dict[str, Any]]:
        return self._get("/entities").get("entities", [])

    def register_entity(self, body: dict[str, Any]) -> dict[str, Any]:
        return self._post("/entities", body)

    def resolve_entity(self, text: str) -> dict[str, Any] | None:
        return self._post("/entities/resolve", {"text": text})

    # -- Collections (RFC-030) -------------------------------------------------

    def list_collections(self, workspace_id: str | None = None) -> list[dict[str, Any]]:
        query = {"workspace_id": workspace_id} if workspace_id else None
        return self._get("/collections", query).get("collections", [])

    def create_collection(self, body: dict[str, Any]) -> dict[str, Any]:
        return self._post("/collections", body)

    def add_collection_document(self, collection_id: str, document_id: str) -> dict[str, Any]:
        return self._post(
            f"/collections/{collection_id}/documents", {"document_id": document_id}
        )

    def remove_collection_document(self, collection_id: str, document_id: str) -> None:
        self._delete(f"/collections/{collection_id}/documents/{document_id}")

    def collection_documents(self, collection_id: str) -> list[str]:
        return self._get(f"/collections/{collection_id}/documents").get("documents", [])

    # -- Agents (RFC-060) ------------------------------------------------------

    def list_agents(self, workspace_id: str | None = None) -> list[dict[str, Any]]:
        query = {"workspace_id": workspace_id} if workspace_id else None
        return self._get("/agents", query).get("agents", [])

    def upsert_agent(self, body: dict[str, Any]) -> dict[str, Any]:
        """Create or update a declared agent identity."""
        return self._post("/agents", body).get("agent", {})

    def get_agent(self, agent_id: str, workspace_id: str = "default") -> dict[str, Any]:
        return self._get(f"/agents/{agent_id}", {"workspace_id": workspace_id}).get("agent", {})

    def set_agent_enabled(
        self, agent_id: str, enabled: bool, workspace_id: str = "default"
    ) -> dict[str, Any]:
        return self._post(
            f"/agents/{agent_id}/enabled",
            {"enabled": enabled, "workspace_id": workspace_id},
        ).get("agent", {})

    # -- Replay / simulation / evaluation (RFC-040/075/086) --------------------

    def replay_trace(
        self, trace_id: str, policy: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"policy": policy} if policy else {}
        return self._post(f"/replay/{trace_id}", body)

    def evaluate_replays(
        self,
        *,
        limit: int = 20,
        workspace_id: str | None = None,
        policy: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {"limit": limit}
        if workspace_id:
            body["workspace"] = workspace_id
        if policy:
            body["policy"] = policy
        return self._post("/evaluation/replay", body)

    # -- Deletion audit (RFC-087) ----------------------------------------------

    def list_deletions(
        self,
        *,
        document_id: str | None = None,
        workspace_id: str | None = None,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        query: dict[str, Any] = {"limit": limit}
        if document_id:
            query["document_id"] = document_id
        if workspace_id:
            query["workspace_id"] = workspace_id
        return self._get("/deletions", query).get("deletions", [])

    # -- Policies (RFC-034) ----------------------------------------------------

    def list_policies(self) -> dict[str, Any]:
        return self._get("/policies")

    def policy_versions(self, kind: str) -> list[dict[str, Any]]:
        return self._get(f"/policies/{kind}/versions").get("versions", [])

    def submit_policy(self, kind: str, body: dict[str, Any]) -> dict[str, Any]:
        return self._put(f"/policies/{kind}", body)

    def rollback_policy(self, kind: str, version: int) -> dict[str, Any]:
        return self._post(f"/policies/{kind}/rollback", {"version": version})

    # -- Memory lifecycle (RFC-070) --------------------------------------------

    def memory_lifecycle(self, apply: bool = False) -> dict[str, Any]:
        return self._post("/memory/lifecycle", {"apply": apply})

    # -- Federated source catalog (RFC-063) ------------------------------------

    def sources(self) -> dict[str, Any]:
        return self._get("/sources")

    # -- Source discovery (RFC-050) -------------------------------------------

    def discover_sources(self, workspace_id: str | None = None) -> dict[str, Any]:
        body = {"workspace_id": workspace_id} if workspace_id else {}
        return self._post("/sources/discover", body)

    def list_source_discoveries(
        self, *, status: str | None = None, limit: int = 100
    ) -> list[dict[str, Any]]:
        query: dict[str, Any] = {"limit": limit}
        if status is not None:
            query["status"] = status
        return self._get("/sources/discoveries", query).get("discoveries", [])

    def register_discovered_source(
        self, host: str, workspace_id: str | None = None
    ) -> dict[str, Any]:
        body = {"workspace_id": workspace_id} if workspace_id else {}
        return self._post(f"/sources/discoveries/{host}/register", body)

    def dismiss_discovered_source(self, host: str) -> dict[str, Any]:
        return self._post(f"/sources/discoveries/{host}/dismiss", {})

    # -- Analytics (RFC-026/027/028) ------------------------------------------

    # -- Analytics (RFC-026/027/028) ------------------------------------------

    def dashboard_overview(
        self,
        *,
        workspace_id: str | None = None,
        agent_id: str | None = None,
        provider_id: str | None = None,
        search_mode: str | None = None,
        quality_status: str | None = None,
        start: str | None = None,
        end: str | None = None,
        interval: Interval = "day",
    ) -> dict[str, Any]:
        """Overview Read Model. Filters are pushed into SQL; the response meta
        declares which were applied and which the dataset cannot apply."""
        query: dict[str, Any] = {"interval": interval}
        for key, value in (
            ("workspace_id", workspace_id),
            ("agent_id", agent_id),
            ("provider_id", provider_id),
            ("search_mode", search_mode),
            ("quality_status", quality_status),
            ("from", start),
            ("to", end),
        ):
            if value is not None:
                query[key] = value
        return self._get("/api/v1/dashboard/overview", query)

    def dashboard_dataset(self, path: str, **query: Any) -> dict[str, Any]:
        return self._get(f"/api/v1/dashboard/{path.lstrip('/')}", query)

    def dashboard_export(self, format: Literal["json", "csv"] = "json") -> str | dict[str, Any]:
        text = self._request("GET", f"/api/v1/dashboard/export?format={format}", parse="text")
        if format == "csv":
            return text
        return json.loads(text)

    # -- Transport ------------------------------------------------------------

    def _get(self, path: str, query: dict[str, Any] | None = None) -> dict[str, Any]:
        suffix = ""
        if query:
            from urllib.parse import urlencode

            suffix = "?" + urlencode({k: v for k, v in query.items() if v is not None})
        text = self._request("GET", f"{path}{suffix}", parse="text")
        return json.loads(text)

    def _post(self, path: str, body: dict[str, Any]) -> dict[str, Any]:
        text = self._request(
            "POST", path, body=json.dumps(body).encode(), content_type="application/json", parse="text"
        )
        return json.loads(text)

    def _put(self, path: str, body: dict[str, Any]) -> dict[str, Any]:
        text = self._request(
            "PUT", path, body=json.dumps(body).encode(), content_type="application/json", parse="text"
        )
        return json.loads(text)

    def _delete(self, path: str) -> None:
        self._request("DELETE", path)

    def _url(self, path: str) -> str:
        base = self.base_url.rstrip("/")
        if self.base_path:
            base = f"{base}{self.base_path}"
        return f"{base}{path}"

    def _request(
        self,
        method: str,
        path: str,
        *,
        body: bytes | None = None,
        content_type: str | None = None,
        parse: Literal["json", "text"] = "json",
    ) -> str:
        headers: dict[str, str] = {}
        if content_type:
            headers["content-type"] = content_type
        if self.token:
            headers["authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(
            self._url(path), data=body, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                text = response.read().decode()
                status = response.status
        except urllib.error.HTTPError as error:  # noqa: PERF203 - status is the point
            text = error.read().decode()
            status = error.code
        except urllib.error.URLError as error:
            raise SearchOSError(
                f"gateway unreachable at {self.base_url}: {error.reason}", 0
            ) from error

        if status >= 400:
            code = None
            message = text
            try:
                payload = json.loads(text)
                inner = payload.get("error") or {}
                message = inner.get("message") or inner.get("code") or text
                code = inner.get("code")
            except json.JSONDecodeError:
                pass
            raise SearchOSError(f"{status}: {message}", status, code)
        if parse == "text":
            return text
        return json.loads(text)


def _parse_sse(raw: str) -> list[dict[str, Any]]:
    """Parse `event:` / `data:` frames from the RFC-056 search stream."""
    events: list[dict[str, Any]] = []
    for block in raw.replace("\r\n", "\n").split("\n\n"):
        event = "done"
        data: list[str] = []
        for line in block.split("\n"):
            if line.startswith("event:"):
                event = line[6:].strip()
            elif line.startswith("data:"):
                data.append(line[5:].lstrip())
        if not data:
            continue
        payload = "\n".join(data)
        try:
            parsed: Any = json.loads(payload)
        except json.JSONDecodeError:
            parsed = payload
        events.append({"event": event, "data": parsed})
    return events


__all__ = ["SearchOSClient", "SearchOSError"]
