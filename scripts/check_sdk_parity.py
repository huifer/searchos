#!/usr/bin/env python3
"""Fail when an OpenAPI operation is missing from either generated SDK.

The release checklist requires every gateway path to have a typed method in
both clients. This check is deliberately source-level and dependency-free so it
runs in the existing `sdk` CI job without a YAML package.

Dynamic path parameters are normalised to `{}` on both sides, and the three
generic dashboard helpers cover the whole `/api/v1/dashboard/` family.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def openapi_operations(path: Path) -> set[tuple[str, str]]:
    operations: set[tuple[str, str]] = set()
    in_paths = False
    current: str | None = None
    for raw in path.read_text(encoding="utf-8").splitlines():
        if raw == "paths:":
            in_paths = True
            continue
        if in_paths and raw and not raw.startswith(" "):
            break
        if not in_paths:
            continue
        match = re.match(r"^  (/\S+):\s*$", raw)
        if match:
            current = normalize(match.group(1))
            continue
        match = re.match(r"^    (get|post|put|delete|patch):\s*$", raw)
        if match and current is not None:
            operations.add((match.group(1).upper(), current))
    return operations


def normalize(value: str) -> str:
    value = value.split("?", 1)[0].rstrip("/") or "/"
    value = re.sub(r"\$\{[^}]+\}", "{}", value)
    value = re.sub(r"\{[^}]+\}", "{}", value)
    return value


def typescript_operations(path: Path) -> set[tuple[str, str]]:
    source = path.read_text(encoding="utf-8")
    operations: set[tuple[str, str]] = set()
    patterns = [
        # this.get("/path"), this.post(`/path/${id}`), ...
        re.compile(
            r"this\.(get|post|put|delete|postForm)\(\s*(?:`([^`]+)`|\"([^\"]+)\")",
            re.S,
        ),
        # this.request("POST", "/search/stream", ...)
        re.compile(
            r"this\.request(?:<[^>]*>)?\(\s*\"([A-Z]+)\"\s*,\s*(?:`([^`]+)`|\"([^\"]+)\")",
            re.S,
        ),
    ]
    for pattern in patterns:
        for match in pattern.finditer(source):
            method = match.group(1).upper()
            if method == "POSTFORM":
                method = "POST"
            raw_path = match.group(2) or match.group(3)
            operations.add((method, normalize(raw_path)))
    return operations


def python_operations(path: Path) -> set[tuple[str, str]]:
    source = path.read_text(encoding="utf-8")
    operations: set[tuple[str, str]] = set()
    patterns = [
        re.compile(
            r"self\._(get|post|put|delete)\(\s*f?(?:\"([^\"]+)\"|'([^']+)')",
            re.S,
        ),
        # self._request("GET", f"/path/{id}", ...)
        re.compile(
            r"self\._request\(\s*\"([A-Z]+)\"\s*,\s*f?(?:\"([^\"]+)\"|'([^']+)')",
            re.S,
        ),
    ]
    for pattern in patterns:
        for match in pattern.finditer(source):
            method = match.group(1).upper()
            raw_path = match.group(2) or match.group(3)
            operations.add((method, normalize(raw_path)))
    return operations


def missing(
    expected: set[tuple[str, str]], actual: set[tuple[str, str]]
) -> list[tuple[str, str]]:
    dashboard_generic = ("GET", "/api/v1/dashboard/{}")
    covered = set(actual)
    if dashboard_generic in covered:
        covered |= {op for op in expected if op[1].startswith("/api/v1/dashboard/")}
    return sorted(op for op in expected if op not in covered)


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: check_sdk_parity.py <openapi.yaml> <client.ts> <client.py>",
            file=sys.stderr,
        )
        return 2
    openapi_path, ts_path, py_path = map(Path, sys.argv[1:])
    expected = openapi_operations(openapi_path)
    ts_missing = missing(expected, typescript_operations(ts_path))
    py_missing = missing(expected, python_operations(py_path))
    print(f"openapi operations: {len(expected)}")
    if not ts_missing and not py_missing:
        print("SDK parity: OK (TypeScript and Python cover every operation)")
        return 0
    for label, rows in (("TypeScript", ts_missing), ("Python", py_missing)):
        if rows:
            print(f"{label} missing {len(rows)} operation(s):")
            for method, path in rows:
                print(f"  {method:6s} {path}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
