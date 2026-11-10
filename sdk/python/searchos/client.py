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

    def _url(self, path: str) -> str:
        base = self.base_url.rstrip("/")
        if self.base_path:
            base = f"{base}{self.base_path}"
        return f"{base}{path}"

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


__all__ = ["SearchOSClient", "SearchOSError"]
